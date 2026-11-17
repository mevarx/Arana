use crate::{cdp, launch, private_dir};
use reach_core::{ReachError, Result};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout, Instant},
};

const BRIDGE: &str = "http://127.0.0.1:19825";
const CONTEXT_KEY: &str = "opencli_context_id_v1";
const EXTENSION_ID: &str = "ildkmabpimmkaediidaifkhjpohdnifk";
const EXTENSION_URL: &str =
    "https://chromewebstore.google.com/detail/opencli/ildkmabpimmkaediidaifkhjpohdnifk";

fn setup(message: &str) -> ReachError {
    ReachError::new("NEED_SETUP", message)
}
fn tools(state: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let node = reach_tools::registry::resolve("node", state).ok_or_else(|| setup("Node.js is required for the OpenCLI Bridge; install Node and OpenCLI, then retry browser setup"))?;
    let cli = reach_tools::registry::resolve("opencli", state).ok_or_else(|| {
        setup("OpenCLI is not installed; install OpenCLI, then retry browser setup")
    })?;
    let exe = fs::canonicalize(&cli).unwrap_or(cli.clone());
    let mut roots = exe.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
    roots.push(
        cli.parent()
            .unwrap_or(Path::new("."))
            .join("node_modules/@jackwener/opencli"),
    );
    let root = roots
        .into_iter()
        .find(|p| p.join("package.json").is_file() && p.join("dist/src/daemon.js").is_file())
        .ok_or_else(|| {
            setup("Unsupported OpenCLI package layout; reinstall the official OpenCLI package")
        })?;
    let package: Value =
        serde_json::from_slice(&fs::read(root.join("package.json")).map_err(ReachError::from)?)
            .map_err(|_| setup("OpenCLI package metadata is invalid"))?;
    if package["version"].as_str() != Some("2.0.3") {
        return Err(setup(
            "OpenCLI package version does not match this Bridge integration; install OpenCLI 2.0.3",
        ));
    }
    Ok((node, cli, root))
}

async fn status() -> Result<Value> {
    status_at(BRIDGE).await
}
async fn status_at(base: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(ReachError::from)?;
    let response = client
        .get(format!("{base}/status"))
        .header("X-OpenCLI", "1")
        .send()
        .await
        .map_err(|_| setup("OpenCLI Bridge is not running"))?;
    let mut response = response
        .error_for_status()
        .map_err(|_| setup("OpenCLI Bridge status is unavailable"))?;
    const MAX_STATUS_BYTES: usize = 128 * 1024;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_STATUS_BYTES as u64)
    {
        return Err(setup("OpenCLI Bridge status exceeds the metadata budget"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| setup("Could not read OpenCLI Bridge status"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_STATUS_BYTES {
            return Err(setup("OpenCLI Bridge status exceeds the metadata budget"));
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| setup("OpenCLI Bridge status is invalid"))?;
    if value["ok"] != true {
        return Err(setup("OpenCLI Bridge status is incompatible"));
    }
    Ok(value)
}

async fn ensure_bridge(state: &Path, budget: Duration) -> Result<Option<Child>> {
    let (node, _cli, root) = tools(state)?;
    if let Ok(existing) = status().await {
        if existing["daemonVersion"].as_str() != Some("2.0.3") {
            return Err(setup(
                "An incompatible OpenCLI Bridge is running; it was left unchanged. Close it yourself, then retry.",
            ));
        }
        return Ok(None);
    }
    let mut command = Command::new(node);
    command.arg(root.join("dist/src/daemon.js"));
    for (key, value) in reach_tools::registry::execution_env("opencli", state) {
        command.env(key, value);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| setup("Could not start the OpenCLI Bridge"))?;
    let deadline = Instant::now() + budget.min(Duration::from_secs(10));
    loop {
        if let Ok(value) = status().await {
            if value["daemonVersion"].as_str() != Some("2.0.3") {
                let _ = child.kill().await;
                return Err(setup(
                    "A different OpenCLI Bridge won the startup race; Reach stopped only its own daemon",
                ));
            }
            if value["pid"].as_u64() == Some(u64::from(child.id().unwrap_or_default())) {
                return Ok(Some(child));
            }
            let _ = child.wait().await;
            return Ok(None);
        }
        if child.try_wait().map_err(ReachError::from)?.is_some() {
            return Err(setup("OpenCLI Bridge exited before becoming ready"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            return Err(setup("Timed out starting OpenCLI Bridge"));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

const EXTENSION_VERSION: &str = "1.0.27";
const EXTENSION_ASSET: &str =
    "https://github.com/jackwener/OpenCLI/releases/download/v2.0.3/opencli-extension-v1.0.27.zip";
const EXTENSION_SHA256: &str = "8636d42a0fe44d7ba3b0603dbcd9581e8269ccbca950136d0dfc0ffc39a8a213";
fn extension_dir(state: &Path) -> PathBuf {
    crate::browser_root(state).join("extension")
}
fn extension_manifest(dir: &Path) -> Result<()> {
    let manifest: Value = serde_json::from_str(&reach_core::state::read_private(
        &dir.join("manifest.json"),
        64 * 1024,
    )?)
    .map_err(|_| setup("OpenCLI extension manifest is invalid"))?;
    if manifest["name"] != "OpenCLI"
        || manifest["version"] != EXTENSION_VERSION
        || manifest["manifest_version"] != 3
    {
        return Err(setup(
            "OpenCLI extension identity or version does not match the pinned release",
        ));
    }
    Ok(())
}
async fn unpack_extension(archive: &Path, state: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let parent = crate::browser_root(state);
    private_dir(&parent)?;
    let dir = extension_dir(state);
    reach_core::state::ensure_safe(&dir)?;
    if dir.exists() {
        return Err(setup(
            "Extension target is occupied; unrecorded files were left unchanged",
        ));
    }
    let staging = tempfile::Builder::new()
        .prefix("extension-")
        .tempdir_in(&parent)?;
    let unpacked = staging.path().join("payload");
    let files = reach_tools::download::extract_archive(archive, &unpacked).await?;
    extension_manifest(&unpacked)?;
    let mut records = Vec::new();
    for path in files {
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(&unpacked)
            .map_err(|_| setup("Invalid extension archive path"))?;
        let destination = dir.join(relative);
        reach_core::state::ensure_safe(&destination)?;
        let hash = format!("{:x}", Sha256::digest(fs::read(&path)?));
        let owned = destination
            .strip_prefix(state)
            .map_err(|_| setup("Extension target escaped state"))?
            .to_string_lossy()
            .replace('\\', "/");
        records.push((owned, hash));
    }
    reach_core::state::Store::new(state).mutate(|data| {
        for (path, hash) in &records {
            data["owned_paths"][path] =
                json!({"owner":"arana","scope":"private","kind":"file","sha256":hash});
        }
    })?;
    fs::rename(unpacked, &dir)?;
    Ok(dir)
}
async fn download_extension(state: &Path, budget: Duration) -> Result<PathBuf> {
    let _lock = reach_core::state::FileLock::try_acquire(state, "opencli-extension")?;
    let dir = extension_dir(state);
    reach_core::state::ensure_safe(&dir)?;
    if dir.exists() {
        extension_manifest(&dir)?;
        return Ok(dir);
    }
    let archive = reach_tools::download::download_file(
        EXTENSION_ASSET,
        state,
        EXTENSION_SHA256,
        32 * 1024 * 1024,
        budget,
    )
    .await?;
    unpack_extension(&archive, state).await
}

fn context_id(worker_url: &str, response: &Value) -> Option<String> {
    let url = url::Url::parse(worker_url).ok()?;
    if url.scheme() != "chrome-extension" {
        return None;
    }
    let id = url.host_str()?;
    if id.len() != 32 || !id.bytes().all(|c| (b'a'..=b'p').contains(&c)) {
        return None;
    }
    let value = &response["result"]["value"];
    if value["id"].as_str() != Some(id) {
        return None;
    }
    if id != EXTENSION_ID && (value["name"] != "OpenCLI" || value["version"] != EXTENSION_VERSION) {
        return None;
    }
    value["context"]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.len() <= 128)
        .map(str::to_owned)
}
fn cookie_api(package: &Path) -> Result<PathBuf> {
    let path = package.join("dist/src/browser/page.js");
    if path.is_file() {
        Ok(path)
    } else {
        Err(setup(
            "This OpenCLI version does not expose its Page cookie API",
        ))
    }
}
async fn bound_context(state: &Path) -> Result<Option<String>> {
    if !crate::running(state).await || !crate::owned(state).await? {
        return Ok(None);
    }
    let targets = cdp::targets(crate::port(state)).await?;
    for target in targets {
        let url = target["url"].as_str().unwrap_or("");
        if target["type"] != "service_worker" || !url.starts_with("chrome-extension://") {
            continue;
        }
        let key = serde_json::to_string(CONTEXT_KEY)
            .unwrap_or_else(|_| "\"opencli_context_id_v1\"".to_owned());
        let expression = format!("chrome.storage.local.get({key}).then(r => ({{id:chrome.runtime.id,name:chrome.runtime.getManifest().name,version:chrome.runtime.getManifest().version,context:r[{key}] || null}}))");
        let value = cdp::call(
            crate::port(state),
            "Runtime.evaluate",
            json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
            target["id"].as_str(),
        )
        .await?;
        if let Some(id) = context_id(url, &value) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

pub(super) async fn maintain(state: &Path, budget: Duration) -> Result<Option<Child>> {
    ensure_bridge(state, budget).await
}
pub(super) async fn stop_owned(child: &mut Option<Child>) {
    if let Some(mut proc) = child.take() {
        if proc.try_wait().ok().flatten().is_none() {
            let _ = proc.kill().await;
        }
        let _ = timeout(Duration::from_secs(3), proc.wait()).await;
    }
}

pub async fn prepare(state: &Path, budget: Duration) -> Result<()> {
    private_dir(state)?;
    let (_node, _cli, _pkg) = tools(state)?;
    if let Ok(existing) = status().await {
        if existing["daemonVersion"].as_str() != Some("2.0.3") {
            return Err(setup(
                "An incompatible OpenCLI Bridge is running; it was left unchanged",
            ));
        }
    }
    let _extension = download_extension(state, budget).await?;
    if crate::running(state).await {
        if !crate::owned(state).await? {
            return Err(ReachError::new(
                "BUSY",
                "Configured CDP port is occupied by an unmanaged browser; it was not touched",
            ));
        }
        #[cfg(unix)]
        if bound_context(state).await?.is_none() {
            crate::stop(state).await?;
            let deadline = Instant::now() + budget.min(Duration::from_secs(10));
            while crate::running(state).await {
                if Instant::now() >= deadline {
                    return Err(setup(
                        "Could not restart the dedicated browser to load the OpenCLI extension",
                    ));
                }
                sleep(Duration::from_millis(100)).await;
            }
            let _browser = launch(state, false, "about:blank").await?;
        }
    } else {
        let url = if cfg!(windows) {
            EXTENSION_URL
        } else {
            "about:blank"
        };
        let _browser = launch(state, cfg!(windows), url).await?;
    }
    let deadline = Instant::now() + budget;
    loop {
        let own = bound_context(state).await?;
        if let Some(own) = own {
            let bridge = status().await?;
            if bridge["daemonVersion"].as_str() != Some("2.0.3") {
                return Err(setup("OpenCLI Bridge version changed during browser setup"));
            }
            if bridge["profiles"].as_array().is_some_and(|p| {
                p.iter().any(|v| {
                    v["contextId"].as_str() == Some(&own) && v["extensionConnected"] == true
                })
            }) {
                reach_core::state::Store::new(state)
                    .mutate(|data| data["opencli_context"] = json!(own))?;
                return Ok(());
            }
        }
        if cfg!(windows) {
            return Err(ReachError::new(
                "NEED_SETUP",
                "Add the OpenCLI extension to the Reach browser, then run browser setup again; no connection was assumed",
            ));
        }
        if Instant::now() >= deadline {
            return Err(setup(
                "OpenCLI extension did not connect to the dedicated browser before the setup deadline",
            ));
        }
        sleep(Duration::from_millis(200)).await;
    }
}

fn clean_cookie(row: &Value, domains: &[String], now: i64) -> Option<Value> {
    if row["name"].as_str().is_none_or(str::is_empty) || row.get("value").is_none() {
        return None;
    }
    let domain = row["domain"].as_str()?;
    if !domains
        .iter()
        .any(|d| crate::cookie_domain_matches(domain, d))
    {
        return None;
    }
    if row["expires"]
        .as_f64()
        .is_some_and(|x| x > 0.0 && x <= now as f64)
    {
        return None;
    }
    let mut out = json!({});
    for key in [
        "name", "value", "domain", "path", "secure", "httpOnly", "sameSite",
    ] {
        if let Some(v) = row.get(key) {
            out[key] = v.clone();
        }
    }
    out["expires"] = row.get("expires").cloned().unwrap_or(json!(-1));
    Some(out)
}

fn cookie_command(
    node: &Path,
    page: &Path,
    source: &str,
    domain: &str,
    budget: Duration,
    state: &Path,
) -> Result<reach_tools::runner::CommandSpec> {
    let module =
        url::Url::from_file_path(page).map_err(|_| setup("OpenCLI cookie API path is invalid"))?;
    let import=format!("import {{Page}} from {};const p=new Page('arana-site-session',undefined,process.argv[1]);console.log(JSON.stringify(await p.getCookies({{domain:process.argv[2]}})));",serde_json::to_string(module.as_str()).map_err(|_|setup("OpenCLI cookie API path cannot be encoded"))?);
    let mut spec = reach_tools::runner::CommandSpec::new(node, "OpenCLI site-cookie read");
    spec.args = vec![
        "--input-type=module".into(),
        "-e".into(),
        import,
        source.into(),
        domain.into(),
    ];
    spec.env = reach_tools::registry::execution_env("opencli", state);
    spec.timeout = budget;
    spec.max_output_bytes = 2 * 1024 * 1024;
    Ok(spec)
}
pub async fn reuse(state: &Path, domains: &[String], budget: Duration) -> Result<Value> {
    let deadline = Instant::now() + budget;
    if domains.is_empty()
        || domains
            .iter()
            .any(|d| d.trim().is_empty() || d.contains('/') || d.contains('@'))
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Cookie reuse requires explicit domain names",
        ));
    }
    let store = reach_core::state::Store::new(state).load().map_err(|_| {
        setup("No connected source profile is selected; choose one with browser use")
    })?;
    let source = store["local_browser_source"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            setup("No source browser selected; choose a connected profile with browser use ID")
        })?;
    let owned_context = store["opencli_context"]
        .as_str()
        .ok_or_else(|| setup("Reach browser has no confirmed OpenCLI extension binding"))?;
    if source == owned_context {
        return Err(setup(
            "The selected source is the Reach-owned destination, not a reusable source profile",
        ));
    }
    let bridge = status().await?;
    if bridge["daemonVersion"].as_str() != Some("2.0.3") {
        return Err(setup(
            "OpenCLI Bridge version is incompatible; source was not read",
        ));
    }
    if !bridge["profiles"].as_array().is_some_and(|p| {
        p.iter()
            .any(|v| v["contextId"].as_str() == Some(source) && v["extensionConnected"] == true)
    }) {
        return Err(setup(
            "The selected source browser is not connected; no other profile was chosen",
        ));
    }
    if !crate::running(state).await
        || !crate::owned(state).await?
        || bound_context(state).await?.as_deref() != Some(owned_context)
    {
        return Err(setup("Dedicated Reach browser binding is unavailable"));
    }
    let (node, _cli, pkg) = tools(state)?;
    let page = cookie_api(&pkg)?;
    let mut gathered = Vec::new();
    for domain in domains {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ReachError::new(
                "TIMEOUT",
                "OpenCLI site-cookie read timed out",
            ));
        }
        let spec = cookie_command(&node, &page, source, domain, remaining, state)?;
        let output = reach_tools::runner::run_captured(spec).await?;
        if !output.success() {
            return Err(setup(
                "Could not read requested cookies from the selected source",
            ));
        }
        let parsed: Value = serde_json::from_str(&output.stdout)
            .map_err(|_| setup("Selected source returned invalid cookie data"))?;
        let rows = parsed
            .as_array()
            .ok_or_else(|| setup("Selected source returned invalid cookie data"))?;
        gathered.extend(
            rows.iter()
                .filter_map(|r| clean_cookie(r, domains, chrono::Utc::now().timestamp())),
        );
    }
    let count = crate::import_site_cookies(state, gathered.clone(), domains).await?;
    Ok(
        json!({"copied_cookies":count,"domains":domains,"source_profile":source,"source_changed":false,"verified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_status_uses_the_requested_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let n = socket.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /status "));
            let body = r#"{"ok":true,"daemonVersion":"2.0.3","profiles":[]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let result = status_at(&endpoint).await;
        server.abort();
        assert!(
            result.is_ok(),
            "local fixture endpoint was ignored: {result:?}"
        );
    }
    #[tokio::test]
    async fn extension_install_checks_manifest_and_preserves_occupied_target() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("extension.zip");
        let file = fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(br#"{"name":"OpenCLI","version":"1.0.27","manifest_version":3}"#)
            .unwrap();
        zip.finish().unwrap();
        let root = temp.path().join("state");
        let installed = unpack_extension(&archive, &root).await.unwrap();
        assert!(installed.join("manifest.json").is_file());
        fs::write(installed.join("user-file"), "preserve").unwrap();
        assert!(unpack_extension(&archive, &root).await.is_err());
        assert_eq!(
            fs::read_to_string(installed.join("user-file")).unwrap(),
            "preserve"
        );
        let state = reach_core::state::Store::new(&root).load().unwrap();
        assert_eq!(
            state["owned_paths"]["browser/extension/manifest.json"]["owner"],
            "arana"
        );
    }
    #[tokio::test]
    async fn wrong_extension_version_is_rejected_before_install() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("wrong.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        zip.start_file("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(br#"{"name":"OpenCLI","version":"99.0.0","manifest_version":3}"#)
            .unwrap();
        zip.finish().unwrap();
        assert!(unpack_extension(&archive, temp.path()).await.is_err());
        assert!(!extension_dir(temp.path()).exists());
    }
    #[test]
    fn unpacked_extension_context_can_be_bound_without_the_store_id() {
        let id = "abcdefghijklmnopabcdefghijklmnop";
        let value = json!({"result":{"value":{"id":id,"name":"OpenCLI","version":"1.0.27","context":"owned-context"}}});
        assert_eq!(
            context_id(
                &format!("chrome-extension://{id}/dist/background.js"),
                &value
            )
            .as_deref(),
            Some("owned-context")
        );
        let mut other = value.clone();
        other["result"]["value"]["name"] = json!("Different extension");
        assert!(context_id(
            &format!("chrome-extension://{id}/dist/background.js"),
            &other
        )
        .is_none());
        assert!(context_id("https://example.com/", &value).is_none());
    }
    #[test]
    fn cookie_api_is_resolved_from_the_actual_pinned_npm_layout() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dist/src/browser/page.js");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "fixture module").unwrap();
        assert_eq!(cookie_api(root.path()).unwrap(), path);
    }
    #[tokio::test]
    async fn bridge_status_rejects_oversized_metadata() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await;
            let body = format!("{{\"ok\":true,\"extra\":\"{}\"}}", "x".repeat(256 * 1024));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        let result = status_at(&endpoint).await;
        server.abort();
        assert!(
            result.is_err(),
            "oversized local Bridge response was accepted"
        );
    }
    #[test]
    fn cookie_read_command_uses_bounded_runner_and_explicit_source_arguments() {
        let root = tempfile::tempdir().unwrap();
        let command = cookie_command(
            Path::new("node"),
            &root.path().join("page.js"),
            "chosen-profile",
            "example.test",
            Duration::from_millis(25),
            root.path(),
        )
        .unwrap();
        assert_eq!(command.timeout, Duration::from_millis(25));
        assert_eq!(command.max_output_bytes, 2 * 1024 * 1024);
        assert_eq!(&command.args[3..], &["chosen-profile", "example.test"]);
    }
    #[test]
    fn cookie_copy_is_explicit_domain_boundary_scoped_and_live_only() {
        let domains = vec!["example.test".to_owned()];
        let now = 1_800_000_000;
        assert!(clean_cookie(&json!({"name":"ok","value":"secret","domain":".sub.example.test","expires":1_900_000_000}), &domains, now).is_some());
        assert!(clean_cookie(
            &json!({"name":"no","value":"x","domain":"notexample.test","expires":1_900_000_000}),
            &domains,
            now
        )
        .is_none());
        assert!(clean_cookie(
            &json!({"name":"old","value":"x","domain":"example.test","expires":1}),
            &domains,
            now
        )
        .is_none());
        assert!(clean_cookie(
            &json!({"name":"no-value","domain":"example.test"}),
            &domains,
            now
        )
        .is_none());
    }
}
