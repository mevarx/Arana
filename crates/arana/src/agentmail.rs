//! Additive AgentMail support for agent-owned email aliases.
//!
//! This is deliberately outside the frozen 14-channel / 40-action catalog and
//! outside public MCP. It shells out to the `agentmail` CLI with an explicit
//! argument vector, reads the API key only from `AGENTMAIL_API_KEY`, and never
//! writes the key to Arana state, logs, or output.
//!
//! Scope: an agent can create or inspect its own alias, read mail, and pull
//! verification codes out of an inbox. Arana does not sign into third-party
//! sites, click links, follow instructions found in email, or send mail in
//! this first integration.

use async_trait::async_trait;
use reach_core::{envelope, ReachError, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncBufReadExt, BufReader};
use tokio::process::Command;

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_TEXT_CHARS: usize = 8_000;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

static CODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:\d{4,8}|[A-Z0-9]{6,10})\b").expect("OTP token regex")
});

const USAGE: &str = "usage: arana agentmail status | inbox list | inbox create --username NAME --client-id ID [--display-name NAME] | messages --inbox-id ID [--label LABEL] [--limit N] | message --inbox-id ID --message-id ID | otp --inbox-id ID [--sender SUBSTRING] [--limit N] | login --url URL --inbox-id ID --allow-domain HOST --approve-login [--password-stdin] [--sender SUBSTRING] [--email-selector CSS] [--code-selector CSS] [--password-selector CSS] [--request-selector CSS] [--submit-selector CSS]";

fn started_envelope(action: &str, started: Instant, error: ReachError) -> Value {
    envelope(
        Some("agentmail"),
        Some(action),
        None,
        vec![],
        started.elapsed().as_millis() as u64,
        vec![],
        Some(error),
    )
}

fn bad(action: &str, started: Instant, message: impl Into<String>) -> Value {
    started_envelope(
        action,
        started,
        ReachError::new("BAD_INPUT", message),
    )
}

fn ok(action: &str, started: Instant, items: Vec<Value>, backend: &str) -> Value {
    envelope(
        Some("agentmail"),
        Some(action),
        Some(backend),
        items,
        started.elapsed().as_millis() as u64,
        vec![json!({"backend": "agentmail", "result": "ok", "command": action})],
        None,
    )
}

fn cli_path() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("ARANA_AGENTMAIL_BIN") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Some(path);
        }
    }
    which::which("agentmail").ok()
}

fn api_key() -> Result<String> {
    let key = std::env::var("AGENTMAIL_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string();
    if key.is_empty() {
        let mut error = ReachError::new("NEED_SETUP", "AGENTMAIL_API_KEY is not set");
        error.hint = "Create a key at https://console.agentmail.to and export AGENTMAIL_API_KEY. Arana never stores it.".into();
        return Err(error);
    }
    Ok(key)
}

fn redact(text: &str, key: &str) -> String {
    text.replace(key, "[redacted]")
}

/// Run one AgentMail CLI command. The key is passed only to this child process.
async fn call(arguments: &[&str], key: &str) -> Result<Value> {
    let Some(binary) = cli_path() else {
        let mut error = ReachError::new("NEED_SETUP", "agentmail CLI was not found");
        error.hint = "Install agentmail-cli and make `agentmail` available on PATH, or set ARANA_AGENTMAIL_BIN to its absolute path.".into();
        return Err(error);
    };
    let mut child = match Command::new(binary)
        .args(arguments)
        .args(["--format", "json"])
        .env("AGENTMAIL_API_KEY", key)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Err(ReachError::from(error)),
    };
    let status = match tokio::time::timeout(CLI_TIMEOUT, child.wait()).await {
        Ok(result) => result.map_err(ReachError::from)?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ReachError::new(
                "TIMEOUT",
                "agentmail CLI did not respond within 30 seconds",
            ));
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        pipe.take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut stdout)
            .await
            .map_err(ReachError::from)?;
    }
    if let Some(pipe) = child.stderr.take() {
        pipe.take(4097)
            .read_to_end(&mut stderr)
            .await
            .map_err(ReachError::from)?;
    }
    if stdout.len() > MAX_OUTPUT_BYTES {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "agentmail CLI returned more than 1 MiB of output",
        ));
    }
    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr);
        let message = redact(stderr_text.trim(), key);
        let message: String = message.chars().take(300).collect();
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            if message.is_empty() {
                format!("agentmail CLI exited with {status}")
            } else {
                message
            },
        ));
    }
    let value: Value = serde_json::from_slice(&stdout).map_err(|error| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!("agentmail CLI returned invalid JSON: {error}"),
        )
    })?;
    if let Some(error) = reject_api_error(&value, key) {
        return Err(error);
    }
    Ok(value)
}

fn reject_api_error(value: &Value, key: &str) -> Option<ReachError> {
    let error = value.get("error")?;
    if error.is_null() {
        return None;
    }
    let (name, message) = match error {
        Value::Object(map) => (
            map.get("name")
                .or_else(|| map.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("UPSTREAM_BROKEN")
                .to_string(),
            map.get("message")
                .or_else(|| map.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("AgentMail returned an error")
                .to_string(),
        ),
        Value::String(text) => ("UPSTREAM_BROKEN".to_string(), text.clone()),
        _ => ("UPSTREAM_BROKEN".to_string(), "AgentMail returned an error".to_string()),
    };
    let message: String = redact(&message, key).chars().take(300).collect();
    Some(ReachError::new(name, message))
}

fn list_field(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            for name in ["inboxes", "messages", "items", "data"] {
                if let Some(Value::Array(items)) = map.get(name) {
                    return items.clone();
                }
            }
            vec![]
        }
        _ => vec![],
    }
}

fn first_str(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .map(str::to_string)
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_TEXT_CHARS).collect()
}

fn compact_message(message: &Value) -> Value {
    let text = first_str(message, &["extracted_text", "text", "extracted_html"])
        .map(|text| clip(&text))
        .unwrap_or_default();
    let from = message
        .get("from")
        .or_else(|| message.get("sender"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "id": first_str(message, &["id", "message_id"]),
        "thread_id": first_str(message, &["thread_id"]),
        "from": from,
        "subject": first_str(message, &["subject"]),
        "received_at": message.get("received_at").or_else(|| message.get("timestamp")).cloned().unwrap_or(Value::Null),
        "text": text,
    })
}

const ID_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-@+";

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.chars().all(|c| ID_CHARS.contains(c))
}

fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_client_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Strict flag parser. Value flags consume the next argument; boolean flags do
/// not. Repeats and unknown flags fail closed. No value reaches a shell.
fn parse_flags(
    args: &[String],
    allowed: &[&str],
    booleans: &[&str],
) -> Result<(std::collections::HashMap<String, String>, std::collections::HashSet<String>)> {
    let mut values = std::collections::HashMap::new();
    let mut switches = std::collections::HashSet::new();
    let mut index = 0;
    while index < args.len() {
        let name = args[index].as_str();
        let Some(key) = name.strip_prefix("--") else {
            return Err(ReachError::new("BAD_INPUT", format!("unexpected argument: {name}")));
        };
        if booleans.contains(&key) {
            if !switches.insert(key.to_string()) {
                return Err(ReachError::new("BAD_INPUT", format!("--{key} was repeated")));
            }
            index += 1;
            continue;
        }
        if !allowed.contains(&key) {
            return Err(ReachError::new("BAD_INPUT", format!("unknown option: --{key}")));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| ReachError::new("BAD_INPUT", format!("--{key} requires a value")))?
            .clone();
        if value.starts_with("--") {
            return Err(ReachError::new("BAD_INPUT", format!("--{key} requires a value")));
        }
        if values.insert(key.to_string(), value).is_some() {
            return Err(ReachError::new("BAD_INPUT", format!("--{key} was repeated")));
        }
        index += 2;
    }
    Ok((values, switches))
}

fn flags(args: &[String], allowed: &[&str]) -> Result<std::collections::HashMap<String, String>> {
    parse_flags(args, allowed, &[]).map(|(values, _)| values)
}

fn limit(parsed: &std::collections::HashMap<String, String>) -> Result<usize> {
    match parsed.get("limit") {
        None => Ok(10),
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=25).contains(value))
            .ok_or_else(|| ReachError::new("BAD_INPUT", "--limit must be an integer between 1 and 25")),
    }
}

fn require_id<'a>(parsed: &'a std::collections::HashMap<String, String>, name: &str) -> Result<&'a str> {
    let value = parsed
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| ReachError::new("BAD_INPUT", format!("--{name} is required")))?;
    if !valid_id(value) {
        return Err(ReachError::new("BAD_INPUT", format!("--{name} is not a valid AgentMail id")));
    }
    Ok(value)
}

async fn status(started: Instant) -> Value {
    let cli = cli_path();
    let key = api_key().ok();
    let mut item = json!({
        "cli_found": cli.is_some(),
        "cli_path": cli.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "key_configured": key.is_some(),
        "authenticated": false,
        "inboxes": 0,
    });
    if let (Some(_), Some(key)) = (cli.as_ref(), key.as_ref()) {
        match call(&["inboxes", "list"], key).await {
            Ok(value) => {
                let count = list_field(&value).len();
                item["authenticated"] = json!(true);
                item["inboxes"] = json!(count);
            }
            Err(error) => {
                item["error"] = json!({"code": error.code, "message": error.message});
            }
        }
    } else {
        item["hint"] = json!(USAGE);
    }
    ok("status", started, vec![item], "agentmail-cli")
}

async fn inbox(args: &[String], started: Instant) -> Value {
    let Some(action) = args.first().map(String::as_str) else {
        return bad("inbox", started, "usage: arana agentmail inbox list | create ...");
    };
    let key = match api_key() {
        Ok(key) => key,
        Err(error) => return started_envelope("inbox", started, error),
    };
    match action {
        "list" => {
            if args.len() != 1 {
                return bad("inbox list", started, "inbox list takes no arguments");
            }
            match call(&["inboxes", "list"], &key).await {
                Ok(value) => ok("inbox list", started, list_field(&value), "agentmail-cli"),
                Err(error) => started_envelope("inbox list", started, error),
            }
        }
        "create" => {
            let parsed = match flags(&args[1..], &["username", "client-id", "display-name"]) {
                Ok(parsed) => parsed,
                Err(error) => return started_envelope("inbox create", started, error),
            };
            let Some(username) = parsed.get("username").map(String::as_str) else {
                return bad("inbox create", started, "--username is required");
            };
            if !valid_username(username) {
                return bad("inbox create", started, "--username must be 1-40 lowercase letters, digits, or hyphens");
            }
            let Some(client_id) = parsed.get("client-id").map(String::as_str) else {
                return bad("inbox create", started, "--client-id is required; use a stable value for retries");
            };
            if !valid_client_id(client_id) {
                return bad("inbox create", started, "--client-id must be 1-80 letters, digits, dots, underscores, or hyphens");
            }
            if let Some(name) = parsed.get("display-name") {
                if name.is_empty() || name.chars().count() > 100 {
                    return bad("inbox create", started, "--display-name must be 1-100 characters");
                }
            }
            let mut argv = vec!["inboxes", "create", "--username", username, "--client-id", client_id];
            if let Some(name) = parsed.get("display-name") {
                argv.extend(["--display-name", name]);
            }
            match call(&argv, &key).await {
                Ok(value) => ok("inbox create", started, vec![value], "agentmail-cli"),
                Err(error) => started_envelope("inbox create", started, error),
            }
        }
        other => bad(
            "inbox",
            started,
            format!("unknown inbox action: {other}; use list or create"),
        ),
    }
}

async fn messages(args: &[String], started: Instant) -> Value {
    let parsed = match flags(args, &["inbox-id", "label", "limit"]) {
        Ok(parsed) => parsed,
        Err(error) => return started_envelope("messages", started, error),
    };
    let inbox_id = match require_id(&parsed, "inbox-id") {
        Ok(id) => id,
        Err(error) => return started_envelope("messages", started, error),
    };
    let cap = match limit(&parsed) {
        Ok(cap) => cap,
        Err(error) => return started_envelope("messages", started, error),
    };
    let key = match api_key() {
        Ok(key) => key,
        Err(error) => return started_envelope("messages", started, error),
    };
    let mut argv = vec!["inboxes:messages", "list", "--inbox-id", inbox_id];
    if let Some(label) = parsed.get("label") {
        if !valid_id(label) {
            return bad("messages", started, "--label contains unsupported characters");
        }
        argv.extend(["--label", label]);
    }
    match call(&argv, &key).await {
        Ok(value) => {
            let items: Vec<Value> = list_field(&value)
                .iter()
                .take(cap)
                .map(compact_message)
                .collect();
            ok("messages", started, items, "agentmail-cli")
        }
        Err(error) => started_envelope("messages", started, error),
    }
}

async fn message(args: &[String], started: Instant) -> Value {
    let parsed = match flags(args, &["inbox-id", "message-id"]) {
        Ok(parsed) => parsed,
        Err(error) => return started_envelope("message", started, error),
    };
    let inbox_id = match require_id(&parsed, "inbox-id") {
        Ok(id) => id,
        Err(error) => return started_envelope("message", started, error),
    };
    let message_id = match require_id(&parsed, "message-id") {
        Ok(id) => id,
        Err(error) => return started_envelope("message", started, error),
    };
    let key = match api_key() {
        Ok(key) => key,
        Err(error) => return started_envelope("message", started, error),
    };
    match call(
        &["inboxes:messages", "get", "--inbox-id", inbox_id, "--message-id", message_id],
        &key,
    )
    .await
    {
        Ok(value) => ok("message", started, vec![compact_message(&value)], "agentmail-cli"),
        Err(error) => started_envelope("message", started, error),
    }
}

fn code_candidates(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for capture in CODE_RE.captures_iter(text) {
        let token = capture.get(0).map(|m| m.as_str()).unwrap_or("");
        let digits = token.chars().filter(|c| c.is_ascii_digit()).count();
        let letters = token.chars().filter(|c| c.is_ascii_uppercase()).count();
        let plausible = (token.chars().all(|c| c.is_ascii_digit()) && (4..=8).contains(&digits))
            || (letters > 0 && digits > 0 && token.len() >= 6 && (1..=8).contains(&digits));
        if plausible && seen.insert(token.to_string()) {
            found.push(token.to_string());
        }
    }
    found
}

fn excerpt(text: &str, token: &str) -> String {
    let Some(position) = text.find(token) else {
        return clip(text);
    };
    let before: String = text[..position].chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
    let after: String = text[position..].chars().take(90).collect();
    format!("{before}{after}")
}

/// Fetch bounded OTP candidates from an inbox. Reused by `otp` and the
/// approved login flow so there is only one code-extraction implementation.
async fn fetch_codes(
    inbox_id: &str,
    sender: Option<&str>,
    cap: usize,
) -> Result<Vec<Value>> {
    if let Some(sender) = sender {
        if !valid_id(sender) {
            return Err(ReachError::new("BAD_INPUT", "--sender contains unsupported characters"));
        }
    }
    let key = api_key()?;
    let value = call(&["inboxes:messages", "list", "--inbox-id", inbox_id], &key).await?;
    let mut items = Vec::new();
    for raw in list_field(&value) {
        let message = compact_message(&raw);
        if let Some(sender) = sender {
            let from = message["from"].to_string().to_lowercase();
            if !from.contains(&sender.to_lowercase()) {
                continue;
            }
        }
        let mut haystack = message["subject"].as_str().unwrap_or("").to_string();
        haystack.push('\n');
        haystack.push_str(message["text"].as_str().unwrap_or(""));
        for code in code_candidates(&haystack) {
            items.push(json!({
                "code": code,
                "message_id": message["id"],
                "from": message["from"],
                "subject": message["subject"],
                "received_at": message["received_at"],
                "excerpt": excerpt(&haystack, &code),
            }));
            if items.len() >= cap {
                break;
            }
        }
        if items.len() >= cap {
            break;
        }
    }
    Ok(items)
}

async fn otp(args: &[String], started: Instant) -> Value {
    let parsed = match flags(args, &["inbox-id", "sender", "limit"]) {
        Ok(parsed) => parsed,
        Err(error) => return started_envelope("otp", started, error),
    };
    let inbox_id = match require_id(&parsed, "inbox-id") {
        Ok(id) => id,
        Err(error) => return started_envelope("otp", started, error),
    };
    let cap = match limit(&parsed) {
        Ok(cap) => cap,
        Err(error) => return started_envelope("otp", started, error),
    };
    let sender = parsed.get("sender").map(String::as_str).filter(|s| !s.is_empty());
    match fetch_codes(inbox_id, sender, cap).await {
        Ok(items) => ok("otp", started, items, "agentmail-cli"),
        Err(error) => started_envelope("otp", started, error),
    }
}

// ── Explicitly approved login automation ──────────────────────────────────

const DEFAULT_EMAIL_SELECTOR: &str =
    "input[type='email'], input[name='email'], input[name='username'], input[id='email']";
const DEFAULT_CODE_SELECTOR: &str =
    "input[autocomplete='one-time-code'], input[name*='code' i], input[name*='otp' i], input[id*='code' i]";
const DEFAULT_PASSWORD_SELECTOR: &str = "input[type='password']";
const DEFAULT_SUBMIT_SELECTOR: &str =
    "button[type='submit'], input[type='submit'], button[data-testid='submit']";

fn valid_selector(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 300
        && value.chars().all(|c| !c.is_control())
}

#[derive(Debug)]
struct LoginPlan {
    url: String,
    host: String,
    alias: String,
    email_selector: String,
    code_selector: String,
    password_selector: String,
    submit_selector: String,
    request_selector: String,
    use_password: bool,
    sender: Option<String>,
}

fn build_login_plan(
    values: &std::collections::HashMap<String, String>,
    switches: &std::collections::HashSet<String>,
) -> Result<LoginPlan> {
    if !switches.contains("approve-login") {
        let mut error = ReachError::new(
            "BAD_INPUT",
            "autonomous login requires explicit --approve-login on every invocation",
        );
        error.hint = "Only run this for a site you trust, with an AgentMail alias you own.".into();
        return Err(error);
    }
    let raw_url = values
        .get("url")
        .ok_or_else(|| ReachError::new("BAD_INPUT", "--url is required"))?;
    let normalized = reach_tools::ssrf::normalize_public_http_url(raw_url)
        .map_err(|e| ReachError::new("BAD_INPUT", format!("invalid login URL: {}", e.message)))?;
    let parsed = url::Url::parse(&normalized)
        .map_err(|e| ReachError::new("BAD_INPUT", format!("invalid login URL: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(ReachError::new(
            "BAD_INPUT",
            "autonomous login requires an https URL",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ReachError::new("BAD_INPUT", "login URL has no host"))?
        .to_ascii_lowercase();
    let allowed = values
        .get("allow-domain")
        .ok_or_else(|| ReachError::new("BAD_INPUT", "--allow-domain is required"))?
        .to_ascii_lowercase();
    if host != allowed {
        return Err(ReachError::new(
            "BAD_INPUT",
            format!("--allow-domain {allowed} does not match the URL host {host}"),
        ));
    }
    let alias = values
        .get("inbox-id")
        .ok_or_else(|| ReachError::new("BAD_INPUT", "--inbox-id is required"))?;
    if !valid_id(alias) || !alias.contains('@') {
        return Err(ReachError::new("BAD_INPUT", "--inbox-id must be an AgentMail email alias"));
    }
    let use_password = switches.contains("password-stdin");
    if use_password && values.contains_key("sender") {
        return Err(ReachError::new(
            "BAD_INPUT",
            "--sender only applies to OTP login; do not combine it with --password-stdin",
        ));
    }
    let selector = |name: &str, default: &str| -> Result<String> {
        match values.get(name) {
            None => Ok(default.to_string()),
            Some(value) if valid_selector(value) => Ok(value.clone()),
            Some(_) => Err(ReachError::new(
                "BAD_INPUT",
                format!("--{name} must be a CSS selector of at most 300 printable characters"),
            )),
        }
    };
    Ok(LoginPlan {
        url: normalized,
        host,
        alias: alias.clone(),
        email_selector: selector("email-selector", DEFAULT_EMAIL_SELECTOR)?,
        code_selector: selector("code-selector", DEFAULT_CODE_SELECTOR)?,
        password_selector: selector("password-selector", DEFAULT_PASSWORD_SELECTOR)?,
        submit_selector: selector("submit-selector", DEFAULT_SUBMIT_SELECTOR)?,
        request_selector: selector("request-selector", DEFAULT_SUBMIT_SELECTOR)?,
        use_password,
        sender: values.get("sender").cloned(),
    })
}

#[async_trait]
trait LoginBrowser {
    async fn begin(&self, root: &Path) -> Result<u16>;
    async fn navigate(&self, port: u16, url: &str) -> Result<()>;
    async fn wait_ready(&self, port: u16) -> Result<()>;
    async fn focus(&self, port: u16, selector: &str) -> Result<()>;
    async fn type_text(&self, port: u16, text: &str) -> Result<()>;
    async fn click(&self, port: u16, selector: &str) -> Result<()>;
    async fn cookies(&self, port: u16, host: &str) -> Result<Vec<Value>>;
}

struct DedicatedBrowser;

#[async_trait]
impl LoginBrowser for DedicatedBrowser {
    async fn begin(&self, root: &Path) -> Result<u16> {
        reach_browser::automation_begin(root).await
    }
    async fn navigate(&self, port: u16, url: &str) -> Result<()> {
        reach_browser::automation_navigate(port, url).await
    }
    async fn wait_ready(&self, port: u16) -> Result<()> {
        reach_browser::automation_wait_ready(port).await
    }
    async fn focus(&self, port: u16, selector: &str) -> Result<()> {
        reach_browser::automation_focus(port, selector).await
    }
    async fn type_text(&self, port: u16, text: &str) -> Result<()> {
        reach_browser::automation_type(port, text).await
    }
    async fn click(&self, port: u16, selector: &str) -> Result<()> {
        reach_browser::automation_click(port, selector).await
    }
    async fn cookies(&self, port: u16, host: &str) -> Result<Vec<Value>> {
        reach_browser::automation_cookies(port, host).await
    }
}

fn redact_secret(error: ReachError, secret: &str) -> ReachError {
    let mut error = error;
    error.message = redact(&error.message, secret);
    error.hint = redact(&error.hint, secret);
    error
}

async fn perform_login<B: LoginBrowser + ?Sized>(
    browser: &B,
    root: &Path,
    plan: &LoginPlan,
    secret: &str,
) -> Result<Value> {
    let run = async {
        let port = browser.begin(root).await?;
        browser.navigate(port, &plan.url).await?;
        browser.wait_ready(port).await?;
        browser.focus(port, &plan.email_selector).await?;
        browser.type_text(port, &plan.alias).await?;
        browser.click(port, &plan.request_selector).await?;
        browser.wait_ready(port).await?;
        let secret_selector = if plan.use_password {
            &plan.password_selector
        } else {
            &plan.code_selector
        };
        browser.focus(port, secret_selector).await?;
        browser.type_text(port, secret).await?;
        browser.click(port, &plan.submit_selector).await?;
        browser.wait_ready(port).await?;
        let cookies = browser.cookies(port, &plan.host).await?;
        Ok(json!({
            "url": plan.url,
            "host": plan.host,
            "alias": plan.alias,
            "method": if plan.use_password { "password" } else { "otp" },
            "steps": ["navigate", "enter_alias", "request_code", "enter_secret", "submit", "read_cookies"],
            "cookie_count": cookies.len(),
            "authenticated_hint": !cookies.is_empty(),
            "next": "Verify with arana check <channel> before using authenticated actions. Cookies alone do not prove login.",
        }))
    };
    run.await.map_err(|error| redact_secret(error, secret))
}

async fn read_password_stdin() -> Result<String> {
    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
        .map_err(ReachError::from)?;
    let password = line.trim_end_matches(['\r', '\n']).to_string();
    if password.is_empty() {
        return Err(ReachError::new(
            "BAD_INPUT",
            "--password-stdin requires one non-empty line on stdin",
        ));
    }
    Ok(password)
}

async fn login(args: &[String], started: Instant, root: &Path) -> Value {
    let (values, switches) = match parse_flags(
        args,
        &[
            "url",
            "inbox-id",
            "allow-domain",
            "sender",
            "email-selector",
            "code-selector",
            "password-selector",
            "submit-selector",
            "request-selector",
        ],
        &["approve-login", "password-stdin"],
    ) {
        Ok(parsed) => parsed,
        Err(error) => return started_envelope("login", started, error),
    };
    let plan = match build_login_plan(&values, &switches) {
        Ok(plan) => plan,
        Err(error) => return started_envelope("login", started, error),
    };
    let secret = if plan.use_password {
        match read_password_stdin().await {
            Ok(password) => password,
            Err(error) => return started_envelope("login", started, error),
        }
    } else {
        match fetch_codes(&plan.alias, plan.sender.as_deref(), 1).await {
            Ok(items) => match items.first().and_then(|item| item["code"].as_str()) {
                Some(code) => code.to_string(),
                None => {
                    return started_envelope(
                        "login",
                        started,
                        ReachError::new(
                            "UPSTREAM_BROKEN",
                            "no verification code found in the AgentMail alias",
                        ),
                    )
                }
            },
            Err(error) => return started_envelope("login", started, error),
        }
    };
    let config = reach_browser::SlotConfig::default();
    let _guard = match reach_browser::Slots::new(root.join("browser-locks"), config)
        .acquire(&plan.host)
        .await
    {
        Ok(guard) => guard,
        Err(error) => return started_envelope("login", started, error),
    };
    match perform_login(&DedicatedBrowser, root, &plan, &secret).await {
        Ok(result) => ok("login", started, vec![result], "agentmail-cli+dedicated-browser"),
        Err(error) => started_envelope("login", started, error),
    }
}

/// Dispatch `arana agentmail ...`.
pub async fn command(args: &[String], root: &Path) -> Value {
    let started = Instant::now();
    let Some(subcommand) = args.first().map(String::as_str) else {
        return bad("agentmail", started, USAGE);
    };
    match subcommand {
        "status" if args.len() == 1 => status(started).await,
        "inbox" => inbox(&args[1..], started).await,
        "messages" => messages(&args[1..], started).await,
        "message" => message(&args[1..], started).await,
        "otp" => otp(&args[1..], started).await,
        "login" => login(&args[1..], started, root).await,
        "help" | "--help" => ok(
            "help",
            started,
            vec![json!({"usage": USAGE, "scope": "Agent-owned email alias, verification-code reading, and explicitly approved login in the dedicated browser; no link following or mail sending"})],
            "agentmail-cli",
        ),
        other => bad("agentmail", started, format!("unknown agentmail action: {other}; {USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn plan() -> LoginPlan {
        LoginPlan {
            url: "https://example.com/login".into(),
            host: "example.com".into(),
            alias: "agent@agentmail.to".into(),
            email_selector: DEFAULT_EMAIL_SELECTOR.into(),
            code_selector: DEFAULT_CODE_SELECTOR.into(),
            password_selector: DEFAULT_PASSWORD_SELECTOR.into(),
            submit_selector: DEFAULT_SUBMIT_SELECTOR.into(),
            request_selector: DEFAULT_SUBMIT_SELECTOR.into(),
            use_password: false,
            sender: None,
        }
    }

    struct FakeBrowser {
        calls: Mutex<Vec<String>>,
    }

    impl FakeBrowser {
        fn new() -> Self {
            Self { calls: Mutex::new(Vec::new()) }
        }
        fn record(&self, entry: String) {
            self.calls.lock().unwrap().push(entry);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LoginBrowser for FakeBrowser {
        async fn begin(&self, _root: &Path) -> Result<u16> {
            self.record("begin".into());
            Ok(9222)
        }
        async fn navigate(&self, _port: u16, url: &str) -> Result<()> {
            self.record(format!("navigate {url}"));
            Ok(())
        }
        async fn wait_ready(&self, _port: u16) -> Result<()> {
            self.record("wait_ready".into());
            Ok(())
        }
        async fn focus(&self, _port: u16, selector: &str) -> Result<()> {
            self.record(format!("focus {selector}"));
            Ok(())
        }
        async fn type_text(&self, _port: u16, text: &str) -> Result<()> {
            self.record(format!("type {}", if text.len() > 20 { "<secret>" } else { text }));
            Ok(())
        }
        async fn click(&self, _port: u16, selector: &str) -> Result<()> {
            self.record(format!("click {selector}"));
            Ok(())
        }
        async fn cookies(&self, _port: u16, host: &str) -> Result<Vec<Value>> {
            self.record(format!("cookies {host}"));
            Ok(vec![json!({"domain": host, "name": "session"})])
        }
    }

    #[test]
    fn login_plan_requires_approval_and_an_exact_domain() {
        let mut values = std::collections::HashMap::new();
        values.insert("url".into(), "https://example.com/login".into());
        values.insert("inbox-id".into(), "agent@agentmail.to".into());
        values.insert("allow-domain".into(), "example.com".into());
        let mut switches = std::collections::HashSet::new();
        assert_eq!(
            build_login_plan(&values, &switches).unwrap_err().code,
            "BAD_INPUT"
        );
        switches.insert("approve-login".into());
        assert_eq!(build_login_plan(&values, &switches).unwrap().host, "example.com");
        values.insert("allow-domain".into(), "evil.test".into());
        assert_eq!(
            build_login_plan(&values, &switches).unwrap_err().code,
            "BAD_INPUT"
        );
    }

    #[test]
    fn login_plan_rejects_http_and_control_characters() {
        let mut values = std::collections::HashMap::new();
        values.insert("url".into(), "http://example.com/login".into());
        values.insert("inbox-id".into(), "agent@agentmail.to".into());
        values.insert("allow-domain".into(), "example.com".into());
        let switches = std::collections::HashSet::from(["approve-login".to_string()]);
        assert_eq!(
            build_login_plan(&values, &switches).unwrap_err().code,
            "BAD_INPUT"
        );
        values.insert("url".into(), "https://example.com/login".into());
        values.insert("email-selector".into(), "input\n{}".into());
        assert_eq!(
            build_login_plan(&values, &switches).unwrap_err().code,
            "BAD_INPUT"
        );
    }

    #[test]
    fn parse_flags_supports_booleans_and_rejects_repeats() {
        let args: Vec<String> = ["--approve-login", "--password-stdin"]
            .iter().map(|s| s.to_string()).collect();
        let (values, switches) = parse_flags(&args, &[], &["approve-login", "password-stdin"]).unwrap();
        assert!(values.is_empty());
        assert!(switches.contains("approve-login"));
        let repeated: Vec<String> = ["--approve-login", "--approve-login"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(
            parse_flags(&repeated, &[], &["approve-login"]).unwrap_err().code,
            "BAD_INPUT"
        );
    }

    #[tokio::test]
    async fn perform_login_uses_the_secret_without_echoing_it() {
        let browser = FakeBrowser::new();
        let secret = "987654";
        let result = perform_login(&browser, Path::new("."), &plan(), secret)
            .await
            .unwrap();
        let text = result.to_string();
        assert!(!text.contains(secret), "{text}");
        assert_eq!(result["authenticated_hint"], true);
        assert_eq!(result["method"], "otp");
        let calls = browser.calls();
        assert!(calls.iter().any(|c| c == "type 987654"), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("navigate https://example.com/login")), "{calls:?}");
        assert!(calls.iter().any(|c| c == "cookies example.com"), "{calls:?}");
    }

    #[test]
    fn redact_secret_scrubs_message_and_hint() {
        let mut error = ReachError::new("UPSTREAM_BROKEN", "failed with am_secret_123");
        error.hint = "retry with am_secret_123".into();
        let redacted = redact_secret(error, "am_secret_123");
        assert!(!redacted.message.contains("am_secret_123"));
        assert!(!redacted.hint.contains("am_secret_123"));
    }
}
