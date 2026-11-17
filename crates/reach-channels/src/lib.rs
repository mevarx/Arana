//! Channel registry and native backends for arana.
//!
//! The registry is data: 14 channels and 40 actions frozen from Agent-Reach
//! 1.6.1 (`tests/golden/channels.json`, `tests/golden/registry.json`). Backends
//! are addressed by their upstream label, so the dispatcher in `reach-core`
//! walks `Action::backends` in order and never needs to know a backend exists.
//!
//! Native (no external runtime) backends live here: web (Jina/Exa), v2ex,
//! github (gh + REST), xueqiu (Tencent quote/search + Xueqiu API) and bilibili
//! `read`/`hot` through the public API. Backends that need an external tool
//! report `NEED_SETUP` when that tool is absent, exactly like upstream, so the
//! fallback chain degrades instead of failing.

pub mod bilibili;
pub mod boss;
pub mod github;
pub mod http;
pub mod media;
pub mod opencli;
pub mod params;
pub mod registry;
pub mod session;
pub mod tool;
pub mod urlsec;
pub mod v2ex;
pub mod web;
pub mod xueqiu;

use http::Endpoints;
use reach_core::ReachError;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub use registry::{channel, registry, route, Action, Channel, Login, Param};

pub type Result<T> = reach_core::Result<T>;

/// Program name used in usage strings.
pub const PROGRAM: &str = "arana";

/// The agent-facing catalog: exactly upstream's `channels` payload, with this
/// program's name in usage strings.
pub fn catalog() -> Value {
    catalog_for(PROGRAM)
}

/// Same catalog rendered for another program name (`agent-reach` reproduces the
/// Python build byte for byte; used by the contract tests).
pub fn catalog_for(program: &str) -> Value {
    let channels: Vec<Value> = registry()
        .iter()
        .map(|channel| {
            let actions: Vec<Value> = channel
                .actions
                .iter()
                .map(|action| {
                    let params: Vec<Value> = action
                        .params
                        .iter()
                        .map(|param| {
                            json!({
                                "name": param.name,
                                "help": param.help,
                                "required": param.required,
                                "type": param.kind,
                                "default": param.default,
                            })
                        })
                        .collect();
                    let labels: Vec<&str> = action
                        .backends
                        .iter()
                        .map(|(label, _)| label.as_str())
                        .collect();
                    json!({
                        "name": action.name,
                        "help": action.help,
                        "usage": usage(program, channel, action),
                        "params": params,
                        "backends": labels,
                        "public_web": action.public_web,
                    })
                })
                .collect();
            json!({
                "name": channel.name,
                "description": channel.description,
                "level": channel.level,
                "login": login_json(channel, program),
                "actions": actions,
            })
        })
        .collect();
    json!({
        "schema_version": 1,
        "ok": true,
        "commands": {
            "read": format!("{program} read <url>  (picks the channel from the URL)"),
            "action": format!("{program} <channel> <action> [args] [--timeout SECONDS]"),
        },
        "channels": channels,
    })
}

fn usage(program: &str, channel: &Channel, action: &Action) -> String {
    let mut parts = vec![
        program.to_string(),
        channel.name.clone(),
        action.name.clone(),
    ];
    for param in &action.params {
        if param.required {
            parts.push(format!("<{}>", param.name));
        } else {
            parts.push(format!("[--{} {}]", param.name, param.name.to_uppercase()));
        }
    }
    parts.join(" ")
}

fn login_json(channel: &Channel, program: &str) -> Value {
    let command = format!("{program} login {}", channel.name);
    if !channel.login_url.is_empty() {
        json!({"via": "browser", "logged_in": null, "command": command})
    } else if !channel.session.is_empty() {
        json!({"via": "session", "logged_in": false, "command": command})
    } else {
        Value::Null
    }
}

/// Execute one backend of one action.
///
/// `backend` is an upstream backend label from `Action::backends`. Errors carry
/// the unified codes; a missing external tool yields `NEED_SETUP` so the caller
/// can fall through to the next backend.
pub async fn execute(
    channel_name: &str,
    action: &str,
    backend: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    execute_with(
        channel_name,
        action,
        backend,
        params,
        timeout,
        state_dir,
        &Endpoints::default(),
    )
    .await
}

/// `execute` with injectable base URLs (local fixtures in tests).
pub async fn execute_with(
    channel_name: &str,
    action: &str,
    backend: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    let selected = channel(channel_name).ok_or_else(|| {
        let valid: Vec<&str> = registry().iter().map(|c| c.name.as_str()).collect();
        ReachError::new(
            "BAD_INPUT",
            format!(
                "unknown channel: {channel_name}; valid: {}",
                valid.join(", ")
            ),
        )
    })?;
    let declared = selected.action(action).ok_or_else(|| {
        let valid: Vec<&str> = selected.actions.iter().map(|a| a.name.as_str()).collect();
        ReachError::new(
            "BAD_INPUT",
            format!(
                "unknown action for {}: {action}; valid: {}",
                selected.name,
                valid.join(", ")
            ),
        )
    })?;
    if !declared.has_backend(backend) && backend != "obscura" && backend != "public-web" {
        let labels: Vec<&str> = declared.backends.iter().map(|(l, _)| l.as_str()).collect();
        return Err(ReachError::new(
            "BAD_INPUT",
            format!(
                "{backend} is not a backend of {}/{}; valid: {}",
                selected.name,
                declared.name,
                labels.join(", ")
            ),
        ));
    }
    // The caller owns the deadline: it already has `Action::timeout` from the
    // registry and must apply it (a slow action such as transcription declares
    // 1800 s). Re-capping here would silently shorten those actions.

    match (selected.name.as_str(), backend) {
        // obscura is an opt-in runtime extension: it is not in the frozen catalog, so the
        // dispatcher only reaches it when a caller explicitly selects it.
        ("web", "obscura") => {
            let target = crate::params::req_str(params, "target")?;
            let url = crate::urlsec::normalize_public_http_url(&target)
                .map_err(|_| ReachError::new("BAD_INPUT", "target must be a public http(s) URL"))?;
            reach_tools::obscura::obscura_read(&url, timeout, state_dir).await
        }
        ("web", "jina") => web::jina_read(params, timeout, endpoints).await,
        ("web", "exa") if action == "search" => web::exa_search(params, timeout, endpoints).await,
        ("web", "exa") => web::exa_read(params, timeout, endpoints).await,
        ("v2ex", "v2ex-api") => v2ex::read(action, params, timeout, endpoints).await,
        ("github", "gh") => github::gh_backend(action, params, timeout).await,
        ("github", "github-api") => {
            github::api_backend(action, params, timeout, endpoints, state_dir).await
        }
        ("xueqiu", "tencent") => xueqiu::tencent_backend(action, params, timeout, endpoints).await,
        ("xueqiu", "xueqiu-api") => {
            xueqiu::api_backend(action, params, timeout, endpoints, state_dir).await
        }
        ("bilibili", "bilibili-api") => {
            bilibili::api_backend(action, params, timeout, endpoints).await
        }
        ("bilibili", "bili-cli") => bilibili::cli_backend(action, params, timeout, state_dir).await,
        ("youtube", "yt-dlp") | ("youtube", "whisper") | ("xiaoyuzhou", "groq-whisper") => {
            media_backend(&selected.name, action, backend, params, timeout, state_dir).await
        }
        (_, "opencli") => {
            opencli::execute(&selected.name, action, params, timeout, state_dir).await
        }
        ("boss", "boss-agent-cli") => boss::cli_backend(action, params, timeout, state_dir).await,
        _ => Err(ReachError::new(
            "BAD_INPUT",
            format!(
                "unknown backend for {}/{}: {backend}",
                selected.name, declared.name
            ),
        )),
    }
}

/// YouTube and Xiaoyuzhou backends live in `media.rs`.
async fn media_backend(
    channel: &str,
    action: &str,
    backend: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    media::execute(channel, action, backend, params, timeout, state_dir).await
}

impl Channel {
    /// Look up one of this channel's actions.
    pub fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.name == name)
    }

    /// Whether this channel can read `url` (upstream `can_handle`).
    pub fn can_handle(&self, url: &str) -> bool {
        if self.name == "web" {
            return true;
        }
        if self.domains.is_empty() {
            return false;
        }
        let domains: Vec<&str> = self.domains.iter().map(String::as_str).collect();
        urlsec::host_matches(url, &domains)
    }
}
