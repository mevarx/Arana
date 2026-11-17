//! Unified contracts and resilient dispatch primitives.
pub mod clock;
pub mod dispatch;
pub mod input;
pub mod state;
#[cfg(windows)]
mod windows_acl;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;
pub type Result<T> = std::result::Result<T, ReachError>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReachError {
    pub code: String,
    pub message: String,
    pub hint: String,
}
impl ReachError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let hint = hint(&code).to_string();
        Self {
            code,
            message: scrub(&message.into()).trim().chars().take(500).collect(),
            hint,
        }
    }
    pub fn is_final(&self) -> bool {
        matches!(
            self.code.as_str(),
            "NOT_FOUND" | "BAD_INPUT" | "PLATFORM_BLOCKED" | "RATE_LIMITED" | "NEED_LOGIN" | "BUSY"
        )
    }
}
impl std::fmt::Display for ReachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ReachError {}
impl From<std::io::Error> for ReachError {
    fn from(e: std::io::Error) -> Self {
        Self::new(
            if e.kind() == std::io::ErrorKind::NotFound {
                "NEED_SETUP"
            } else {
                "UPSTREAM_BROKEN"
            },
            e.to_string(),
        )
    }
}
impl From<reqwest::Error> for ReachError {
    fn from(e: reqwest::Error) -> Self {
        let code = if e.is_timeout() {
            "TIMEOUT"
        } else {
            match e.status().map(|v| v.as_u16()) {
                Some(404) => "NOT_FOUND",
                Some(429) => "RATE_LIMITED",
                _ => "UPSTREAM_BROKEN",
            }
        };
        Self::new(code, e.without_url().to_string())
    }
}
pub fn hint(code: &str) -> &'static str {
    match code{
"BUSY"=>"The shared browser stayed full while this command waited in line, or a login window is open. Retry later; do not switch profiles.",
"PLATFORM_BLOCKED"=>"Stop automation. Keep this profile; the user must resolve the platform verification in the official page. Do not switch backends or retry.",
"NEED_SETUP"=>"This backend is not installed. Ask the user to run `agent-reach doctor` for setup steps.",
"NEED_LOGIN"=>"The attempted saved/local session did not grant access. Offer site-scoped local reuse or let the user log in themselves; never ask for a password.",
"UPSTREAM_BROKEN"=>"This channel is temporarily not working; `message` is what was reported. Tell the user in plain words which platform has a problem, without naming tools; suggest `agent-reach update` only if the message points to an outdated tool; do not retry in a loop.",
"TIMEOUT"=>"The operation timed out. Inspect channel status and the failed stage; do not reopen login windows or repeat requests in a loop.",
"RATE_LIMITED"=>"The platform reported a rate limit. Reach pauses this channel; respect retry_after_seconds and do not switch backends.",
"NOT_FOUND"=>"The requested content does not exist. Check the link or id.",
"BAD_INPUT"=>"Invalid arguments. Run `agent-reach channels` to see valid usage.",_=>""}
}
pub fn scrub(text: &str) -> String {
    static USERINFO: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)(https?://)[^\s/@]+(?::[^\s/@]*)?@").unwrap());
    static SECRET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)([?&](?:api[_-]?key|token|access_token|key|secret|password)=)[^&#\s]+",
        )
        .unwrap()
    });
    let s = USERINFO.replace_all(text, "${1}[redacted]@");
    SECRET.replace_all(&s, "${1}[redacted]").into_owned()
}
pub fn make_item(
    title: Option<String>,
    url: Option<String>,
    author: Option<String>,
    published_at: Option<String>,
    text: Option<String>,
    raw: Value,
) -> Value {
    fn clean(s: Option<String>) -> Option<String> {
        s.filter(|s| !s.is_empty())
    }
    json!({"title":clean(title),"url":clean(url),"author":clean(author),"published_at":clean(published_at),"text":clean(text),"raw":raw})
}
#[allow(clippy::too_many_arguments)]
pub fn envelope(
    channel: Option<&str>,
    action: Option<&str>,
    backend: Option<&str>,
    items: Vec<Value>,
    elapsed_ms: u64,
    attempts: Vec<Value>,
    error: Option<ReachError>,
) -> Value {
    let ok = error.is_none();
    json!({"schema_version":1,"ok":ok,"channel":channel,"action":action,"backend":if ok{backend}else{None},"items":if ok{items}else{vec![]},"meta":{"elapsed_ms":elapsed_ms,"attempts":attempts},"error":error})
}
