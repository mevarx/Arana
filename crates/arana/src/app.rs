mod management;
mod mcp;
use async_trait::async_trait;
use reach_core::{
    clock::Clock,
    dispatch::{self, Backend, PublicRoute, Request},
    input,
    state::{self, Store},
    ReachError, Result,
};
use serde_json::{json, Value};
use std::{
    any::Any,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

struct Runtime {
    root: PathBuf,
    verbose: bool,
    browser: bool,
}
#[async_trait]
impl Backend for Runtime {
    async fn call(&self, c: &str, a: &str, b: &str, p: &Value, t: Duration) -> Result<Vec<Value>> {
        if self.verbose {
            eprintln!(
                "attempt channel={c} action={a} backend={b} budget_ms={}",
                t.as_millis()
            );
        }
        reach_channels::execute(c, a, b, p, t, &self.root).await
    }
    async fn slot(&self, c: &str, clock: &Clock) -> Result<Option<Box<dyn Any + Send>>> {
        if !self.browser {
            return Ok(None);
        }
        let config = reach_browser::SlotConfig {
            wait: Duration::from_secs(120).min(clock.remaining()),
            ..Default::default()
        };
        let guard = reach_browser::Slots::new(self.root.join("browser-locks"), config)
            .acquire(c)
            .await?;
        Ok(Some(Box::new(guard)))
    }
}
fn root() -> PathBuf {
    let raw = std::env::var_os("ARANA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".arana")
        });
    canonical_root(&raw)
}

/// Resolve the symlinks that already exist in the state root's own path, so a
/// root below a platform-symlinked directory (macOS `/var` and `/tmp`) is
/// usable. Links created *inside* the state tree are still refused by the
/// state guard, because only the root is resolved here.
fn canonical_root(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() {
            return match std::fs::canonicalize(&existing) {
                Ok(real) => missing
                    .iter()
                    .rev()
                    .fold(real, |acc, name| acc.join(name.as_os_str())),
                Err(_) => path.to_path_buf(),
            };
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}
fn bad(c: Option<&str>, a: Option<&str>, message: impl Into<String>) -> Value {
    reach_core::envelope(
        c,
        a,
        None,
        vec![],
        0,
        vec![],
        Some(ReachError::new("BAD_INPUT", message)),
    )
}
fn mgmt(a: &str, r: Result<Value>) -> Value {
    match r {
        Ok(v) => {
            let ok = v.get("ok") != Some(&json!(false));
            json!({"schema_version":1,"ok":ok,"action":a,"result":v,"error":if ok{Value::Null}else{serde_json::to_value(ReachError::new("UPSTREAM_BROKEN","One or more live checks failed")).unwrap_or(Value::Null)}})
        }
        Err(e) => json!({"schema_version":1,"ok":false,"action":a,"result":null,"error":e}),
    }
}
fn spec(a: &reach_channels::Action) -> Value {
    json!({"name":a.name,"timeout":a.timeout,"params":a.params.iter().map(|p|json!({"name":p.name,"type":p.kind,"required":p.required,"default":p.default})).collect::<Vec<_>>()})
}
fn scopes_match(target: &str, scopes: &[String]) -> bool {
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    scopes.iter().any(|s| {
        let (h, p) = s.split_once('/').unwrap_or((s, ""));
        (host == h || host.ends_with(&format!(".{h}")))
            && (p.is_empty() || url.path().starts_with(&format!("/{p}")))
    })
}
fn public_route(a: &reach_channels::Action, p: &Value) -> Option<PublicRoute> {
    if a.public_web.is_empty() {
        return None;
    }
    let (action, params) = if matches!(a.name.as_str(), "search" | "jobs") {
        let query = ["query", "location", "city"]
            .iter()
            .filter_map(|k| p[k].as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let scope = a
            .public_web
            .iter()
            .map(|s| format!("site:{s}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        (
            "search",
            json!({"query":format!("({query}) ({scope})"),"limit":p.get("limit").cloned().unwrap_or(json!(5))}),
        )
    } else if a.name == "read" && scopes_match(p["target"].as_str().unwrap_or(""), &a.public_web) {
        ("read", json!({"target":p["target"]}))
    } else {
        return None;
    };
    Some(PublicRoute {
        action: action.into(),
        params,
        scopes: a.public_web.clone(),
    })
}
fn usage_tail(a: &reach_channels::Action) -> String {
    a.params
        .iter()
        .map(|p| {
            if p.required {
                format!("<{}>", p.name)
            } else {
                format!("[--{} {}]", p.name, p.name.to_uppercase())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
async fn action(ch: &reach_channels::Channel, name: &str, args: &[String], root: &Path) -> Value {
    let Some(a) = ch.actions.iter().find(|a| a.name == name) else {
        return bad(
            Some(&ch.name),
            Some(name),
            format!(
                "unknown action '{name}' for {}; valid: {}",
                ch.name,
                ch.actions
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };
    let parsed = match input::parse(&spec(a), args) {
        Ok(v) => v,
        Err(e) => {
            return bad(
                Some(&ch.name),
                Some(name),
                format!(
                    "{}; usage: arana {} {} {}",
                    e.message,
                    ch.name,
                    name,
                    usage_tail(a)
                ),
            )
        }
    };
    invoke_action(
        ch,
        name,
        parsed.params,
        Duration::from_secs_f64(parsed.timeout),
        parsed.backend,
        parsed.verbose,
        root,
    )
    .await
}
/// Execute one action from already-parsed JSON parameters.
///
/// The CLI and the MCP server share this path, so argument validation, the
/// dispatcher, deadlines and the error model are identical. Only the CLI can
/// reach `only_backend` and `verbose`; the MCP layer never passes either.
pub(crate) async fn invoke_action(
    ch: &reach_channels::Channel,
    name: &str,
    params: Value,
    timeout: Duration,
    only_backend: Option<String>,
    verbose: bool,
    root: &Path,
) -> Value {
    let Some(a) = ch.actions.iter().find(|a| a.name == name) else {
        return bad(
            Some(&ch.name),
            Some(name),
            format!(
                "unknown action '{name}' for {}; valid: {}",
                ch.name,
                ch.actions
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };
    let params = match input::validate(&spec(a), &params, timeout.as_secs_f64()) {
        Ok(v) => v,
        Err(e) => {
            return bad(
                Some(&ch.name),
                Some(name),
                format!(
                    "{}; usage: arana {} {} {}",
                    e.message,
                    ch.name,
                    name,
                    usage_tail(a)
                ),
            )
        }
    };
    let mut backends: Vec<_> = a.backends.iter().map(|b| b.0.clone()).collect();
    if ch.name == "web" && a.name == "read" && !backends.iter().any(|b| b == "obscura") {
        backends.insert(0, "obscura".into());
    }
    if only_backend
        .as_ref()
        .is_some_and(|b| !backends.contains(b) && !(b == "public-web" && !a.public_web.is_empty()))
    {
        return bad(
            Some(&ch.name),
            Some(name),
            format!("no backend named '{}'", only_backend.as_deref().unwrap()),
        );
    }
    let store = Store::new(root);
    let saved = match store.load() {
        Ok(v) => v,
        Err(e) => {
            return reach_core::envelope(
                Some(&ch.name),
                Some(name),
                None,
                vec![],
                0,
                vec![],
                Some(e),
            )
        }
    };
    let added = saved["channels"].get(&ch.name).is_some()
        || (ch.level == "default"
            && !saved["removed_defaults"]
                .as_array()
                .unwrap()
                .contains(&json!(ch.name)));
    let browser = a
        .backends
        .iter()
        .any(|b| matches!(b.0.as_str(), "opencli" | "boss-agent-cli"));
    let fingerprint = management::channel_fingerprint(ch, root, &saved);
    let request = Request {
        channel: ch.name.clone(),
        action: name.into(),
        backends,
        params: params.clone(),
        timeout,
        only_backend,
        public: public_route(a, &params),
        added,
        paced: browser,
        fingerprint,
    };
    let runtime = Runtime {
        root: root.into(),
        verbose,
        browser,
    };
    dispatch::run(&runtime, &store, &request).await
}
fn fingerprint(s: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn configure(args: &[String], root: &Path) -> Result<Value> {
    let key = args
        .first()
        .ok_or_else(|| ReachError::new("BAD_INPUT", "configure requires a key"))?
        .replace('-', "_");
    let key = match key.as_str() {
        "groq_key" => "groq_api_key",
        "openai_key" => "openai_api_key",
        s => s,
    };
    if !matches!(
        key,
        "groq_api_key" | "openai_api_key" | "github_token" | "browser_port" | "proxy"
    ) {
        return Err(ReachError::new("BAD_INPUT", "Unknown configuration key"));
    }
    let load = || -> Result<Value> {
        let config = match state::read_private(&root.join("config.yaml"), 1024 * 1024) {
            Ok(s) => serde_yaml::from_str::<Value>(&s).map_err(|_| {
                ReachError::new("BAD_INPUT", "Existing configuration is invalid YAML")
            })?,
            Err(e) if e.code == "NEED_SETUP" => json!({}),
            Err(e) => return Err(e),
        };
        if !config.is_object() {
            return Err(ReachError::new(
                "BAD_INPUT",
                "Configuration must be an object",
            ));
        }
        Ok(config)
    };
    if args.len() == 1 {
        let config = load()?;
        return Ok(json!({"key":key,"configured":config.get(key).is_some_and(|v|!v.is_null())}));
    }
    if args.len() != 2 {
        return Err(ReachError::new(
            "BAD_INPUT",
            "usage: arana configure <key> [value|--stdin]",
        ));
    }
    let value = if args[1] == "--stdin" {
        let mut value = String::new();
        io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_string(&mut value)?;
        if value.len() > 1024 * 1024 {
            return Err(ReachError::new(
                "BAD_INPUT",
                "Configuration input exceeds budget",
            ));
        }
        value.trim_end().to_string()
    } else {
        args[1].clone()
    };
    if value.trim().is_empty() {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Configuration value must not be blank",
        ));
    }
    let value = if key == "browser_port" {
        let n = value
            .parse::<u16>()
            .ok()
            .filter(|n| *n >= 1024)
            .ok_or_else(|| ReachError::new("BAD_INPUT", "browser-port must be 1024..65535"))?;
        json!(n)
    } else {
        json!(value)
    };
    state::private_dir(root)?;
    let mut config_lock = None;
    for _ in 0..250 {
        match state::FileLock::try_acquire(root, "reach-config-write") {
            Ok(guard) => {
                config_lock = Some(guard);
                break;
            }
            Err(e) if e.code == "BUSY" => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    }
    let _lock = config_lock.ok_or_else(|| ReachError::new("BUSY", "configuration is busy"))?;
    // Read only after stdin is consumed and the write lock is held.
    let mut config = load()?;
    config[key] = value;
    let bytes = serde_yaml::to_string(&config)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Could not serialize configuration"))?;
    Store::new(root).mutate(|data|data["owned_paths"]["config.yaml"]=json!({"owner":"arana","scope":"private","kind":"file","sha256":fingerprint(&bytes)}))?;
    state::atomic_write(&root.join("config.yaml"), bytes.as_bytes())?;
    Ok(json!({"key":key,"configured":true}))
}
async fn execute(args: &[String]) -> Value {
    let root = root();
    let Some(cmd) = args.first().map(String::as_str) else {
        return bad(None, None, "missing command; run arana channels");
    };
    match cmd {
        "channels" if args.len() == 1 => reach_channels::catalog(),
        "version" | "--version" if args.len() == 1 => mgmt(
            "version",
            Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"program":"arana","upstream":"1.6.1","upstream_commit":"c70ae6d","source_sha256":env!("ARANA_BUILD_SIGNATURE")}),
            ),
        ),
        "help" | "--help" | "-h" => mgmt(
            "help",
            Ok(
                json!({"usage":"arana setup [--text] | add <channel> | remove <channel> | update [--check|--sync-only] | login|logout <channel> | doctor [--text] | check [channels...] [--text] [--search] [--timeout SECONDS] | uninstall [--dry-run|--yes] | skill [status|link <skills-dir>|unlink <skills-dir>|update] | configure <key> [value|--stdin] | browser setup|status|stop|profiles|use|export|import|logout | mcp | channels | read <url> | <channel> <action> [args] [--timeout SECONDS] [--backend LABEL] [--verbose]","scope":"Full management surface: channels, tools, sessions, skill, migration, confirmed uninstall, and the read-only stdio MCP server"}),
            ),
        ),
        "browser" => {
            let Some(action) = args.get(1).cloned() else {
                return bad(
                    None,
                    Some("browser"),
                    "browser requires setup|status|stop|profiles|use|export|import|logout",
                );
            };
            let result = reach_browser::manage(&action, &args[2..], &root).await;
            let result = match result {
                Ok(value) if action == "setup" => Ok(management::bind_opencli(value, &root).await),
                other => other,
            };
            mgmt("browser", result)
        }
        "doctor" | "check" | "setup" | "update" | "uninstall" => {
            mgmt(cmd, management::run(cmd, &args[1..], &root).await)
        }
        "add" | "remove" | "login" | "logout" | "skill" => {
            let label = if cmd == "skill" {
                match args.get(1) {
                    Some(action) => format!("skill {action}"),
                    None => "skill".to_owned(),
                }
            } else {
                cmd.to_owned()
            };
            mgmt(&label, management::run(cmd, &args[1..], &root).await)
        }
        "configure" => mgmt("configure", configure(&args[1..], &root)),
        "mcp" => bad(
            None,
            Some("mcp"),
            "mcp takes no arguments; it speaks MCP over stdio",
        ),
        "read" => {
            let synthetic = json!({"params":[{"name":"target","type":"str","required":true,"default":null}],"timeout":30});
            let parsed = match input::parse(&synthetic, &args[1..]) {
                Ok(p) => p,
                Err(e) => {
                    return bad(
                        None,
                        Some("read"),
                        format!("{}; usage: arana read <url>", e.message),
                    )
                }
            };
            let target = parsed.params["target"].as_str().unwrap();
            if url::Url::parse(target)
                .ok()
                .is_none_or(|u| !matches!(u.scheme(), "http" | "https") || u.host_str().is_none())
            {
                return bad(None, Some("read"), "read requires a full HTTP(S) URL");
            }
            let ch = reach_channels::route(target);
            let mut forwarded = vec!["--timeout".into(), parsed.timeout.to_string()];
            if let Some(b) = parsed.backend {
                forwarded.extend(["--backend".into(), b]);
            }
            if parsed.verbose {
                forwarded.push("--verbose".into());
            }
            forwarded.extend(["--".into(), target.into()]);
            action(ch, "read", &forwarded, &root).await
        }
        _ => {
            if let Some(ch) = reach_channels::channel(cmd) {
                let Some(a) = args.get(1) else {
                    return bad(
                        Some(cmd),
                        None,
                        format!(
                            "missing action for {cmd}; valid: {}",
                            ch.actions
                                .iter()
                                .map(|a| a.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                };
                action(ch, a, &args[2..], &root).await
            } else {
                bad(
                    None,
                    None,
                    format!(
                        "unknown command '{cmd}'; valid channels: {}",
                        reach_channels::registry()
                            .iter()
                            .map(|c| c.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        }
    }
}
// Match Python json.dumps(..., ensure_ascii=False) separators for the frozen catalog.
fn python_json(v: &Value) -> String {
    match v {
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", serde_json::to_string(k).unwrap(), python_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        _ => v.to_string(),
    }
}
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__browser-watch") {
        if let Err(e) = reach_browser::watcher_entry(&args[1..]).await {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    // `mcp` owns stdout: protocol frames only, diagnostics on stderr.
    if args.len() == 1 && args[0] == "mcp" {
        if let Err(e) = mcp::serve_stdio(&root()).await {
            eprintln!("arana mcp: {e}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    let output = execute(&args).await;
    let ok = output["ok"] == true;
    if args.iter().any(|arg| arg == "--text") {
        let command = args.first().map(String::as_str).unwrap_or("");
        if let Some(text) = management::render_text(command, &output["result"]) {
            let mut stdout = io::stdout().lock();
            if write!(stdout, "{text}").is_err() {
                std::process::exit(1);
            }
            drop(stdout);
            std::process::exit(if ok { 0 } else { 1 });
        }
    }
    let text = python_json(&output);
    let mut stdout = io::stdout().lock();
    if writeln!(stdout, "{text}").is_err() {
        std::process::exit(1);
    }
    drop(stdout);
    std::process::exit(if ok { 0 } else { 1 });
}
