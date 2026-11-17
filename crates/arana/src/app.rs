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
    std::env::var_os("ARANA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".arana")
        })
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
        Ok(v) => json!({"schema_version":1,"ok":true,"action":a,"result":v,"error":null}),
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
                    a.params
                        .iter()
                        .map(|p| if p.required {
                            format!("<{}>", p.name)
                        } else {
                            format!("[--{} {}]", p.name, p.name.to_uppercase())
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            )
        }
    };
    let mut backends: Vec<_> = a.backends.iter().map(|b| b.0.clone()).collect();
    if ch.name == "web" && a.name == "read" && !backends.iter().any(|b| b == "obscura") {
        backends.insert(0, "obscura".into());
    }
    if parsed
        .backend
        .as_ref()
        .is_some_and(|b| !backends.contains(b) && !(b == "public-web" && !a.public_web.is_empty()))
    {
        return bad(
            Some(&ch.name),
            Some(name),
            format!("no backend named '{}'", parsed.backend.as_deref().unwrap()),
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
    let signature = json!([
        env!("CARGO_PKG_VERSION"),
        env!("ARANA_BUILD_SIGNATURE"),
        ch.name,
        saved["tools"],
        saved["session_revisions"][&ch.name],
        state::read_private(&root.join("config.yaml"), 1024 * 1024).ok()
    ])
    .to_string();
    // The signature is only compared/persisted as a digest, never as raw config.
    let fingerprint = fingerprint(&signature);
    let request = Request {
        channel: ch.name.clone(),
        action: name.into(),
        backends,
        params: parsed.params.clone(),
        timeout: Duration::from_secs_f64(parsed.timeout),
        only_backend: parsed.backend,
        public: public_route(a, &parsed.params),
        added,
        paced: browser,
        fingerprint,
    };
    let runtime = Runtime {
        root: root.into(),
        verbose: parsed.verbose,
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
    let mut config = match state::read_private(&root.join("config.yaml"), 1024 * 1024) {
        Ok(s) => serde_yaml::from_str::<Value>(&s)
            .map_err(|_| ReachError::new("BAD_INPUT", "Existing configuration is invalid YAML"))?,
        Err(e) if e.code == "NEED_SETUP" => json!({}),
        Err(e) => return Err(e),
    };
    if !config.is_object() {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Configuration must be an object",
        ));
    }
    if args.len() == 1 {
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
    config[key] = if key == "browser_port" {
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
    let _lock = state::FileLock::try_acquire(root, "reach-config-write")?;
    let bytes = serde_yaml::to_string(&config)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Could not serialize configuration"))?;
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
                json!({"version":env!("CARGO_PKG_VERSION"),"program":"arana","upstream":"1.6.1","upstream_commit":"c70ae6d"}),
            ),
        ),
        "help" | "--help" => mgmt(
            "help",
            Ok(
                json!({"usage":"arana channels | read <url> | <channel> <action> [args] [--timeout SECONDS] [--backend LABEL] [--verbose] | browser setup|status|stop|profiles|use | configure <key> [value|--stdin]","scope":"M0–M3 public beta; full channel management and OpenCLI adapters are M4–M5"}),
            ),
        ),
        "browser" => {
            let Some(action) = args.get(1) else {
                return bad(
                    None,
                    Some("browser"),
                    "browser requires setup|status|stop|profiles|use",
                );
            };
            mgmt(
                "browser",
                reach_browser::manage(action, &args[2..], &root).await,
            )
        }
        "configure" => mgmt("configure", configure(&args[1..], &root)),
        "add" if args.len() == 2 && matches!(args[1].as_str(), "web" | "obscura") => {
            mgmt("add", reach_tools::install_obscura(&root).await)
        }
        "remove" if args.len() == 2 && matches!(args[1].as_str(), "web" | "obscura") => {
            mgmt("remove", reach_tools::remove_obscura(&root))
        }
        "login" if args.len() == 2 => match reach_channels::channel(&args[1]) {
            Some(ch) if !ch.login_url.is_empty() => mgmt(
                "login",
                reach_browser::login(&ch.name, &ch.login_url, &root).await,
            ),
            Some(_) => mgmt(
                "login",
                Err(ReachError::new(
                    "NEED_SETUP",
                    "This channel uses a native login flow; full session management lands in M5",
                )),
            ),
            None => bad(None, Some("login"), "unknown channel"),
        },
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
    let output = execute(&args).await;
    let ok = output["ok"] == true;
    let text = python_json(&output);
    let mut stdout = io::stdout().lock();
    if writeln!(stdout, "{text}").is_err() {
        std::process::exit(1);
    }
    drop(stdout);
    std::process::exit(if ok { 0 } else { 1 });
}
