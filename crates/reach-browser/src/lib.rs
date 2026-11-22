mod cdp;
mod opencli;
#[cfg(any(unix, test))]
mod pipe;
mod slots;
#[cfg(unix)]
mod watcher_launch;
use reach_core::{ReachError, Result};
use serde_json::{json, Value};
pub use slots::{SlotConfig, SlotGuard, Slots};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::{Child, Command},
    time::sleep,
};

pub async fn prepare_opencli(state: &Path, budget: Duration) -> Result<()> {
    opencli::prepare(state, budget).await
}
pub async fn reuse_site(state: &Path, domains: &[String], budget: Duration) -> Result<Value> {
    opencli::reuse(state, domains, budget).await
}

pub fn cookie_domain_matches(cookie: &str, site: &str) -> bool {
    let c = cookie.trim_start_matches('.').to_ascii_lowercase();
    let s = site.trim_start_matches('.').to_ascii_lowercase();
    c == s || c.ends_with(&format!(".{s}"))
}
pub fn filter_live_cookies(rows: Vec<Value>, now: i64) -> Vec<Value> {
    rows.into_iter()
        .filter(|c| {
            c["expires"]
                .as_f64()
                .is_none_or(|expires| expires <= 0.0 || expires > now as f64)
        })
        .collect()
}
fn private_dir(p: &Path) -> Result<()> {
    reach_core::state::private_dir(p)
}
fn browser_root(state: &Path) -> PathBuf {
    state.join("browser")
}
fn port(state: &Path) -> u16 {
    port_for(state, std::env::var("ARANA_BROWSER_PORT").ok().as_deref())
}
fn port_for(state: &Path, override_value: Option<&str>) -> u16 {
    if let Some(port) = override_value
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| (1024..=65535).contains(p))
    {
        return port;
    }
    let path = state.join("config.yaml");
    if reach_core::state::ensure_safe(&path).is_ok() {
        if let Ok(text) = reach_core::state::read_private(&path, 64 * 1024) {
            if let Ok(config) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
                if let Some(value) = config.get("browser_port") {
                    let parsed = value
                        .as_u64()
                        .and_then(|n| u16::try_from(n).ok())
                        .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
                    if let Some(port) = parsed.filter(|p| (1024..=65535).contains(p)) {
                        return port;
                    }
                }
            }
        }
    }
    9222
}
fn profile_path(state: &Path) -> PathBuf {
    browser_root(state).join("profile")
}
fn save_json(path: &Path, value: &Value) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|e| ReachError::new("UPSTREAM_BROKEN", e.to_string()))?;
    reach_core::state::atomic_write(path, &bytes)
}
/// Check the standalone browser launcher’s candidates without starting it.
pub fn chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ARANA_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    #[cfg(windows)]
    {
        for p in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ] {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    #[cfg(not(windows))]
    {
        for p in [
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ] {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}
async fn running(state: &Path) -> bool {
    cdp::endpoint(port(state), "/json/version").await.is_ok()
}
async fn owned(state: &Path) -> Result<bool> {
    let version = cdp::endpoint(port(state), "/json/version").await?;
    let ws = version["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| ReachError::new("UPSTREAM_BROKEN", "Missing CDP websocket"))?;
    let _ = ws;
    let args = cdp::call(
        port(state),
        "Browser.getBrowserCommandLine",
        json!({}),
        None,
    )
    .await?;
    Ok(commandline_matches_profile(
        &args["arguments"],
        &profile_path(state),
    ))
}
fn commandline_matches_profile(args: &Value, profile: &Path) -> bool {
    let Ok(expected) = normalized_profile(profile) else {
        return false;
    };
    args.as_array().is_some_and(|rows| {
        rows.iter().filter_map(Value::as_str).any(|arg| {
            arg.strip_prefix("--user-data-dir=")
                .and_then(|value| normalized_path_str(value).ok())
                .as_deref()
                == Some(expected.as_str())
        })
    })
}
fn normalized_path_str(raw: &str) -> Result<String> {
    let path = PathBuf::from(raw.trim_matches('"'));
    let path = fs::canonicalize(path).map_err(ReachError::from)?;
    Ok(comparison_path(&path.to_string_lossy(), cfg!(windows)))
}
fn comparison_path(raw: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        raw.replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    } else {
        raw.trim_end_matches('/').to_owned()
    }
}
fn normalized_profile(path: &Path) -> Result<String> {
    reach_core::state::ensure_safe(path)?;
    normalized_path_str(&path.to_string_lossy())
}
async fn launch(state: &Path, headed: bool, url: &str) -> Result<Child> {
    let root = browser_root(state);
    private_dir(&root)?;
    let lock_path = root.join("start.lock");
    reach_core::state::ensure_safe(&lock_path)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(ReachError::from)?;
    let start_deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(e)
                if slots::is_lock_contention(&e)
                    && tokio::time::Instant::now() < start_deadline =>
            {
                sleep(Duration::from_millis(100)).await
            }
            Err(e) if slots::is_lock_contention(&e) => {
                return Err(ReachError::new(
                    "BUSY",
                    "Timed out waiting for the browser startup lock",
                ));
            }
            Err(e) => return Err(ReachError::from(e)),
        }
    }
    if running(state).await {
        if owned(state).await? {
            return Err(ReachError::new(
                "BUSY",
                "Dedicated Chromium is already running",
            ));
        }
        return Err(ReachError::new(
            "BUSY",
            "CDP port occupied by an unmanaged browser; it was not touched",
        ));
    }
    #[cfg(unix)]
    {
        watcher_launch::start(state, headed, url).await
    }
    #[cfg(not(unix))]
    {
        let exe = chrome().ok_or_else(|| {
            ReachError::new(
                "NEED_SETUP",
                "Chrome or Chromium was not found; set ARANA_CHROME to its executable",
            )
        })?;
        let profile = profile_path(state);
        private_dir(&profile)?;
        touch(state)?;
        let mut cmd = Command::new(exe);
        cmd.arg(format!("--remote-debugging-port={}", port(state)))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--enable-automation");
        if !headed {
            cmd.arg("--headless=new");
        }
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        let child = cmd
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(ReachError::from)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            if running(state).await {
                if owned(state).await.unwrap_or(false) {
                    spawn_watcher(state)?;
                    return Ok(child);
                } else {
                    return Err(ReachError::new(
                        "UPSTREAM_BROKEN",
                        "Started Chromium could not be verified as the dedicated profile",
                    ));
                }
            }
            sleep(Duration::from_millis(250)).await;
        }
        Err(ReachError::new(
            "TIMEOUT",
            "Dedicated Chromium did not start within 90 seconds",
        ))
    }
}
async fn stop(state: &Path) -> Result<bool> {
    if !running(state).await {
        let _ = fs::remove_file(browser_root(state).join("login-pin.json"));
        return Ok(true);
    }
    if !owned(state).await? {
        return Err(ReachError::new(
            "BUSY",
            "Refusing to stop an unmanaged browser on the CDP port",
        ));
    }
    let _ = export_cookies(state, None).await?;
    let _ = cdp::call(port(state), "Browser.close", json!({}), None).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if !running(state).await {
            let _ = fs::remove_file(browser_root(state).join("login-pin.json"));
            return Ok(true);
        }
        sleep(Duration::from_millis(200)).await;
    }
    Ok(false)
}
async fn export_cookies(state: &Path, domain: Option<&str>) -> Result<Value> {
    if !running(state).await || !owned(state).await? {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Dedicated browser is not running",
        ));
    }
    let mut rows = cdp::cookies(port(state)).await?;
    if let Some(d) = domain {
        rows.retain(|r| cookie_domain_matches(r["domain"].as_str().unwrap_or(""), d));
    }
    Ok(
        json!({"cookies":rows,"exported_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()}),
    )
}
fn rollback_cookies_to_expire(imported: &[Value], backup: &[Value]) -> Vec<Value> {
    let existed: std::collections::HashSet<(String, String, String)> =
        backup.iter().map(cookie_key).collect();
    imported
        .iter()
        .filter(|c| !existed.contains(&cookie_key(c)))
        .map(|c| {
            let mut expired = c.clone();
            expired["expires"] = json!(1);
            expired
        })
        .collect()
}
fn cookie_key(cookie: &Value) -> (String, String, String) {
    (
        cookie["name"].as_str().unwrap_or("").to_owned(),
        cookie["domain"].as_str().unwrap_or("").to_ascii_lowercase(),
        cookie["path"].as_str().unwrap_or("/").to_owned(),
    )
}
async fn import_site_cookies(state: &Path, rows: Vec<Value>, domains: &[String]) -> Result<usize> {
    if !running(state).await || !owned(state).await? {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Dedicated browser is not running",
        ));
    }
    let rows: Vec<Value> = filter_live_cookies(rows, chrono::Utc::now().timestamp())
        .into_iter()
        .filter(|row| {
            domains
                .iter()
                .any(|d| cookie_domain_matches(row["domain"].as_str().unwrap_or(""), d))
        })
        .collect();
    let mut backup = cdp::cookies(port(state)).await?;
    backup.retain(|row| {
        domains
            .iter()
            .any(|d| cookie_domain_matches(row["domain"].as_str().unwrap_or(""), d))
    });
    let mut updates = rollback_cookies_to_expire(&backup, &rows);
    updates.extend(rows.clone());
    if let Err(error) = cdp::call(
        port(state),
        "Storage.setCookies",
        json!({"cookies":updates}),
        None,
    )
    .await
    {
        let newly_written = rollback_cookies_to_expire(&rows, &backup);
        let expire = cdp::call(
            port(state),
            "Storage.setCookies",
            json!({"cookies":newly_written}),
            None,
        )
        .await;
        let restore = cdp::call(
            port(state),
            "Storage.setCookies",
            json!({"cookies":backup}),
            None,
        )
        .await;
        if expire.is_err() || restore.is_err() {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!(
                    "Site cookie import failed and rollback failed: {}",
                    error.message
                ),
            ));
        }
        return Err(error);
    }
    Ok(rows.len())
}

async fn import_cookies(state: &Path, rows: Vec<Value>) -> Result<usize> {
    if !running(state).await || !owned(state).await? {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Dedicated browser is not running",
        ));
    }
    let now = chrono::Utc::now().timestamp();
    let rows = filter_live_cookies(rows, now);
    let backup = cdp::cookies(port(state)).await?;
    let result = cdp::call(
        port(state),
        "Storage.setCookies",
        json!({"cookies":rows}),
        None,
    )
    .await;
    if let Err(error) = result {
        let newly_written = rollback_cookies_to_expire(&rows, &backup);
        let expire = cdp::call(
            port(state),
            "Storage.setCookies",
            json!({"cookies":newly_written}),
            None,
        )
        .await;
        let restore = cdp::call(
            port(state),
            "Storage.setCookies",
            json!({"cookies":backup}),
            None,
        )
        .await;
        if expire.is_err() || restore.is_err() {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!(
                    "Cookie import failed and rollback failed: {}",
                    error.message
                ),
            ));
        }
        return Err(error);
    }
    Ok(rows.len())
}
async fn expire_cookies(state: &Path, domain: &str) -> Result<usize> {
    if !running(state).await || !owned(state).await? {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Dedicated browser is not running",
        ));
    }
    let mut cookies = cdp::cookies(port(state)).await?;
    cookies.retain(|c| cookie_domain_matches(c["domain"].as_str().unwrap_or(""), domain));
    let count = cookies.len();
    if count == 0 {
        return Ok(0);
    }
    let backup = cdp::cookies(port(state)).await?;
    let expired: Vec<Value> = cookies
        .into_iter()
        .map(|mut c| {
            c["expires"] = json!(1);
            c
        })
        .collect();
    if let Err(error) = cdp::call(
        port(state),
        "Storage.setCookies",
        json!({"cookies":expired}),
        None,
    )
    .await
    {
        if cdp::call(
            port(state),
            "Storage.setCookies",
            json!({"cookies":backup}),
            None,
        )
        .await
        .is_err()
        {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!(
                    "Cookie expiry failed and rollback failed: {}",
                    error.message
                ),
            ));
        }
        return Err(error);
    }
    Ok(count)
}

async fn connected_profiles(state_dir: &Path) -> Result<Vec<Value>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(ReachError::from)?;
    let response = client
        .get("http://127.0.0.1:19825/status")
        .header("X-OpenCLI", "1")
        .send()
        .await
        .map_err(|_| {
            ReachError::new(
                "NEED_SETUP",
                "OpenCLI Bridge is not running; local login sources are unavailable",
            )
        })?;
    let value: Value = response
        .error_for_status()
        .map_err(|_| ReachError::new("NEED_SETUP", "OpenCLI Bridge status is unavailable"))?
        .json()
        .await
        .map_err(ReachError::from)?;
    if value["ok"] != true {
        return Err(ReachError::new(
            "NEED_SETUP",
            "OpenCLI Bridge status is incompatible",
        ));
    }
    let dedicated = reach_core::state::Store::new(state_dir)
        .load()
        .ok()
        .and_then(|s| s["opencli_context"].as_str().map(str::to_owned));
    Ok(value["profiles"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p["extensionConnected"] == true && p["contextId"].is_string())
        .map(|p| {
            let id = p["contextId"].clone();
            json!({"id":id,"dedicated":dedicated.as_deref()==id.as_str()})
        })
        .collect())
}
async fn select_source(state_dir: &Path, id: &str) -> Result<Value> {
    let profiles = connected_profiles(state_dir).await?;
    if !profiles
        .iter()
        .any(|p| p["id"].as_str() == Some(id) && p["dedicated"] != true)
    {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Choose a connected local source profile other than the Reach-owned browser",
        ));
    }
    let store = reach_core::state::Store::new(state_dir);
    store.mutate(|data| {
        data["local_browser_source"] = json!(id);
    })?;
    Ok(
        json!({"source_profile":id,"scope":"requested site only","source_changed":false,"dedicated_profile_unchanged":true}),
    )
}

pub fn touch(state: &Path) -> Result<()> {
    let root = browser_root(state);
    private_dir(&root)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    reach_core::state::atomic_write(&root.join("last_used"), now.as_bytes())
}
fn idle_expired(last: Option<u64>, now: u64, timeout: u64, pinned: bool) -> bool {
    !pinned && last.is_some_and(|then| now.saturating_sub(then) >= timeout)
}
#[derive(Clone, Copy, Debug)]
pub struct WatchConfig {
    pub idle_after: Duration,
    pub poll_every: Duration,
}
impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            idle_after: Duration::from_secs(600),
            poll_every: Duration::from_secs(5),
        }
    }
}
/// Hidden watcher entry point; launch mode is used only by POSIX callers.
pub async fn watcher_entry(args: &[String]) -> Result<()> {
    if args.len() == 1 {
        return watch(Path::new(&args[0])).await;
    }
    if args.len() != 5
        || args[1] != "--launch"
        || !matches!(args[2].as_str(), "headed" | "headless")
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Invalid hidden browser-watcher arguments",
        ));
    }
    #[cfg(unix)]
    {
        watcher_launch::run(Path::new(&args[0]), args[2] == "headed", &args[3], &args[4]).await
    }
    #[cfg(not(unix))]
    {
        Err(ReachError::new(
            "BAD_INPUT",
            "Pipe-owned launch is supported on POSIX only",
        ))
    }
}

/// Long-lived hidden CLI entrypoint: `arana __browser-watch <state-dir>` invokes this.
pub async fn watch(state_dir: &Path) -> Result<()> {
    watch_with_config(state_dir, WatchConfig::default()).await
}
fn watcher_lock(root: &Path) -> Result<std::fs::File> {
    let lock_path = root.join("watcher.lock");
    reach_core::state::ensure_safe(&lock_path)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|e| {
        if slots::is_lock_contention(&e) {
            ReachError::new("BUSY", "A browser watcher already owns this profile")
        } else {
            ReachError::from(e)
        }
    })?;
    Ok(lock)
}
pub async fn watch_with_config(state_dir: &Path, config: WatchConfig) -> Result<()> {
    let root = browser_root(state_dir);
    private_dir(&root)?;
    let _lock = match watcher_lock(&root) {
        Ok(v) => v,
        Err(e) if e.code == "BUSY" => return Ok(()),
        Err(e) => return Err(e),
    };
    watch_loop(state_dir, config).await
}
async fn watch_loop(state_dir: &Path, config: WatchConfig) -> Result<()> {
    let root = browser_root(state_dir);
    let mut bridge_child: Option<Child> = None;
    loop {
        let pin = root.join("login-pin.json").is_file();
        if !running(state_dir).await {
            opencli::stop_owned(&mut bridge_child).await;
            if pin {
                let _ = fs::remove_file(root.join("login-pin.json"));
            }
            return Ok(());
        }
        let last = reach_core::state::read_private(&root.join("last_used"), 64)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if idle_expired(last, now, config.idle_after.as_secs(), pin) {
            let _ = stop(state_dir).await?;
            opencli::stop_owned(&mut bridge_child).await;
            return Ok(());
        }
        if bridge_child
            .as_mut()
            .is_none_or(|p| p.try_wait().ok().flatten().is_some())
        {
            bridge_child = opencli::maintain(state_dir, config.poll_every)
                .await
                .unwrap_or(None);
        }
        sleep(config.poll_every).await;
    }
}
fn spawn_watcher(state: &Path) -> Result<()> {
    let exe = std::env::current_exe().map_err(ReachError::from)?;
    let mut cmd = Command::new(exe);
    cmd.arg("__browser-watch").arg(state);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map_err(ReachError::from)?;
    Ok(())
}

/// Browser management entrypoint. Supported actions: setup/status/stop/profiles/use/export/import/expire/logout.
pub async fn manage(action: &str, args: &[String], state_dir: &Path) -> Result<Value> {
    private_dir(state_dir)?;
    let root = browser_root(state_dir);
    private_dir(&root)?;
    if action != "stop" {
        touch(state_dir)?;
    }
    match action {
        "setup" => {
            if running(state_dir).await {
                if owned(state_dir).await? {
                    spawn_watcher(state_dir)?;
                    return Ok(status(state_dir).await);
                }
                return Err(ReachError::new(
                    "BUSY",
                    "CDP port occupied by an unmanaged browser; it was not touched",
                ));
            }
            let _child = launch(state_dir, false, "about:blank").await?;
            Ok(status(state_dir).await)
        }
        "status" => Ok(status(state_dir).await),
        "stop" => Ok(json!({"stopped":stop(state_dir).await?})),
        "profiles" => Ok(json!({"profiles":connected_profiles(state_dir).await?})),
        "use" => {
            let id = args.first().ok_or_else(|| {
                ReachError::new(
                    "BAD_INPUT",
                    "browser use requires a connected local profile ID",
                )
            })?;
            select_source(state_dir, id).await
        }
        "export" => export_cookies(state_dir, args.first().map(String::as_str)).await,
        "import" => {
            let path = args.first().ok_or_else(|| {
                ReachError::new(
                    "BAD_INPUT",
                    "browser import requires JSON cookie-export path",
                )
            })?;
            let metadata = fs::metadata(path).map_err(ReachError::from)?;
            if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    "Cookie export must be a regular file no larger than 2 MiB",
                ));
            }
            let v: Value = serde_json::from_slice(&fs::read(path).map_err(ReachError::from)?)
                .map_err(|e| ReachError::new("BAD_INPUT", e.to_string()))?;
            let rows = v["cookies"].as_array().cloned().ok_or_else(|| {
                ReachError::new("BAD_INPUT", "Cookie export JSON requires a cookies array")
            })?;
            Ok(json!({"imported":import_cookies(state_dir,rows).await?}))
        }
        "expire" | "logout" => {
            let domain = args.first().ok_or_else(|| {
                ReachError::new("BAD_INPUT", "browser expire requires a site domain")
            })?;
            Ok(json!({"expired":expire_cookies(state_dir,domain).await?}))
        }
        _ => Err(ReachError::new(
            "BAD_INPUT",
            "Unknown browser management action",
        )),
    }
}
async fn status(state: &Path) -> Value {
    let running = running(state).await;
    let is_owned = running && owned(state).await.unwrap_or(false);
    let pin = browser_root(state).join("login-pin.json").is_file();
    json!({"running":running,"owned":is_owned,"profile_dir":profile_path(state),"port":port(state),"headless":if is_owned{Some(!pin)}else{None},"login_pinned":pin && is_owned,"idle_close_after_s":600,"transport":if cfg!(unix){"pipe+websocket"}else{"websocket"},"pipe_supported":cfg!(unix)})
}
/// Opens the requested URL in a visible window using the Reach-owned profile and pins it until stop.
pub async fn login(_channel: &str, url: &str, state_dir: &Path) -> Result<Value> {
    let parsed = url::Url::parse(url).map_err(|e| ReachError::new("BAD_INPUT", e.to_string()))?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Login URL must use HTTP or HTTPS",
        ));
    }
    if running(state_dir).await {
        if !owned(state_dir).await? {
            return Err(ReachError::new(
                "BUSY",
                "Unmanaged browser is using the configured CDP port",
            ));
        }
        if !stop(state_dir).await? {
            return Err(ReachError::new(
                "BUSY",
                "Could not close the dedicated browser to open a login window",
            ));
        }
    }
    let root = browser_root(state_dir);
    touch(state_dir)?;
    save_json(
        &root.join("login-pin.json"),
        &json!({"channel":_channel,"pinned":true,"url":url}),
    )?;
    if let Err(error) = launch(state_dir, true, url).await {
        let _ = fs::remove_file(root.join("login-pin.json"));
        return Err(error);
    }
    Ok(json!({"running":true,"headless":false,"pinned":true,"profile_dir":profile_path(state_dir)}))
}

#[cfg(test)]
mod basic_tests {
    use super::*;
    #[test]
    fn profile_comparison_respects_posix_case_sensitivity() {
        assert_ne!(
            comparison_path("/state/Profile", false),
            comparison_path("/state/profile", false)
        );
        assert_eq!(
            comparison_path("C:\\State\\PROFILE", true),
            comparison_path("c:/state/profile", true)
        );
    }
    #[test]
    fn cookie_rollback_expires_new_cookie_before_restoring_backup() {
        let added = json!({"name":"new","domain":".example.com","path":"/"});
        let existing = json!({"name":"old","domain":"example.com","path":"/"});
        let result = rollback_cookies_to_expire(&[added.clone(), existing.clone()], &[existing]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "new");
        assert_eq!(result[0]["expires"], 1);
    }
    #[test]
    fn cookie_domain_boundary() {
        assert!(cookie_domain_matches(".x.example.com", "example.com"));
        assert!(!cookie_domain_matches("notexample.com", "example.com"));
    }
    #[test]
    fn browser_port_uses_private_config_and_env_override_for_tests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "browser_port: 48123\n").unwrap();
        assert_eq!(port_for(dir.path(), None), 48123);
        assert_eq!(port_for(dir.path(), Some("49181")), 49181);
        assert_eq!(port_for(dir.path(), Some("80")), 48123);
    }
    #[test]
    fn fixed_profile_path_ignores_legacy_untrusted_profile_setting() {
        let dir = tempfile::tempdir().unwrap();
        let browser = dir.path().join("browser");
        std::fs::create_dir_all(&browser).unwrap();
        std::fs::write(
            browser.join("browser.json"),
            r#"{"profile":"../../personal-chrome"}"#,
        )
        .unwrap();
        assert_eq!(profile_path(dir.path()), browser.join("profile"));
    }
    #[test]
    fn browser_ownership_requires_exact_user_data_dir_argument() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let expected = format!("--user-data-dir={}", profile.display());
        assert!(commandline_matches_profile(&json!([expected]), &profile));
        assert!(!commandline_matches_profile(
            &json!([format!("--some-flag={}", profile.display())]),
            &profile
        ));
        assert!(!commandline_matches_profile(
            &json!([format!("--user-data-dir={}-copy", profile.display())]),
            &profile
        ));
    }
    #[test]
    fn login_pin_prevents_idle_close_until_unpinned() {
        assert!(!idle_expired(Some(1_000), 1_700, 600, true));
        assert!(idle_expired(Some(1_000), 1_700, 600, false));
        assert!(!idle_expired(None, 1_700, 600, false));
    }
    #[tokio::test]
    async fn watcher_exits_if_browser_is_gone_and_state_is_empty() {
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:49179") else {
            return;
        };
        drop(listener);
        std::env::set_var("ARANA_BROWSER_PORT", "49179");
        let state = tempfile::tempdir().unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            watch_with_config(
                state.path(),
                WatchConfig {
                    idle_after: Duration::ZERO,
                    poll_every: Duration::from_millis(5),
                },
            ),
        )
        .await;
        std::env::remove_var("ARANA_BROWSER_PORT");
        assert!(result.is_ok());
        assert!(result.unwrap().is_ok());
    }
}
