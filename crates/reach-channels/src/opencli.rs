//! Generic OpenCLI adapter for the seven browser-session sites.
//!
//! Ports `backends/opencli.py` and `channels/_opencli_site.py`: one bounded
//! `opencli <site> ... -f json` call with the explicitly bound profile, a
//! YAML/JSON failure taxonomy, and OpenCLI result rows mapped to unified items.
//! Per-site argument and item rules live here because they are the channel
//! contract, not the transport.

use crate::params::{int_param, opt_str, req_str, unix_to_iso};
use crate::tool;
use crate::Result;
use reach_core::{make_item, state::Store, ReachError};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

const LOGIN_HINT: &str = "Run `arana login {channel}` and ask the user to log in themselves in the dedicated browser window it opens. Never log in for them.";

fn login_hint(channel: &str) -> String {
    LOGIN_HINT.replace("{channel}", channel)
}

/// `opencli` program resolution (registry first, then `ARANA_OPENCLI_BIN`).
fn program(state_dir: &Path) -> Result<std::path::PathBuf> {
    tool::program("opencli", "ARANA_OPENCLI_BIN", state_dir)
}

/// The explicit browser binding from `state.json`. Never falls back to a
/// profile the user happens to have; an unbound state is `NEED_SETUP`.
fn environment(state_dir: &Path) -> Result<Vec<(String, String)>> {
    let state = Store::new(state_dir).load()?;
    let profile = state["opencli_context"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ReachError::new(
                "NEED_SETUP",
                "OpenCLI has not connected to the dedicated browser",
            )
        })?;
    Ok(vec![
        ("OPENCLI_PROFILE".into(), profile.to_string()),
        ("OPENCLI_WINDOW".into(), "background".into()),
        ("OPENCLI_CDP_ENDPOINT".into(), String::new()),
        ("OPENCLI_CDP_TARGET".into(), String::new()),
    ])
}

/// One bounded OpenCLI command; no retry. Returns the decoded envelope.
pub async fn call(
    site: &str,
    args: &[String],
    timeout: Duration,
    state_dir: &Path,
    channel: &str,
) -> Result<Value> {
    let env = environment(state_dir)?;
    let program = program(state_dir)?;
    let mut argv = vec![site.to_string()];
    argv.extend(args.iter().cloned());
    argv.extend(["-f".to_string(), "json".to_string()]);
    let output = tool::run(&program, &argv, env, timeout, "opencli").await?;
    let hint = login_hint(channel);
    if !output.success() {
        let text = if output.stdout.trim().is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        return Err(error_from_text(text, Some(&hint)));
    }
    match serde_json::from_str::<Value>(&output.stdout) {
        Ok(Value::Object(object)) if object.get("ok") == Some(&Value::Bool(false)) => {
            Err(error_from_text(&output.stdout, Some(&hint)))
        }
        Ok(value) => Ok(value),
        Err(_) => Err(error_from_text(&output.stdout, Some(&hint))),
    }
}

/// Turn OpenCLI's YAML error block (printed even with `-f json`) into a
/// unified error. Node warnings may precede the envelope.
pub fn error_from_text(text: &str, login_hint: Option<&str>) -> ReachError {
    static START: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?m)^ok: false\s*$").expect("yaml start"));
    let clean = START
        .find(text)
        .map_or(text, |found| &text[found.start()..]);
    let doc = serde_yaml::from_str::<Value>(clean).ok();
    let error = doc
        .as_ref()
        .and_then(|doc| doc.get("error"))
        .cloned()
        .unwrap_or(Value::Null);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| {
            let fallback = text.trim();
            if fallback.is_empty() {
                "OpenCLI failed".to_string()
            } else {
                fallback.to_string()
            }
        });
    let upstream_hint = error
        .get("help")
        .or_else(|| error.get("hint"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hint| !hint.is_empty());
    let low = message.to_lowercase();
    if [
        "环境异常",
        "环境存在异常",
        "browser environment",
        "captcha",
        "验证码",
        "安全验证",
        "account_risk",
    ]
    .iter()
    .any(|word| low.contains(word))
        || matches!(code.as_str(), "ACCOUNT_RISK" | "CAPTCHA_REQUIRED")
    {
        return tool::with_hint(ReachError::new("PLATFORM_BLOCKED", message), upstream_hint);
    }
    if matches!(code.as_str(), "RATE_LIMITED" | "RATE_LIMIT")
        || ["http 429", "too many requests", "访问过于频繁"]
            .iter()
            .any(|word| low.contains(word))
    {
        return ReachError::new("RATE_LIMITED", message);
    }
    static HTTP_404: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\bHTTP 404\b").expect("404 regex"));
    let login_words = [
        "login",
        "log in",
        "logged in",
        "sign in",
        "http 401",
        "登录",
    ];
    if !HTTP_404.is_match(&message)
        && (code.contains("AUTH")
            || code.contains("LOGIN")
            || login_words.iter().any(|w| low.contains(w)))
    {
        return ReachError::new("NEED_LOGIN", message).with_hint_override(login_hint);
    }
    tool::with_hint(ReachError::new("UPSTREAM_BROKEN", message), upstream_hint)
}

trait HintOverride {
    fn with_hint_override(self, hint: Option<&str>) -> Self;
}

impl HintOverride for ReachError {
    fn with_hint_override(mut self, hint: Option<&str>) -> Self {
        if let Some(hint) = hint.filter(|hint| !hint.is_empty()) {
            self.hint = hint.to_string();
        }
        self
    }
}

/// Map OpenCLI result rows to unified items, keeping each row in `raw`.
pub fn items(rows: &Value) -> Vec<Value> {
    let rows = match rows {
        Value::Object(map) => {
            let listed = ["items", "data"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_array));
            match listed {
                Some(list) => list.as_slice(),
                None => {
                    let fallback = ["items", "data"]
                        .iter()
                        .find_map(|key| map.get(*key))
                        .unwrap_or(rows);
                    return vec![item(fallback)];
                }
            }
        }
        Value::Array(list) => list.as_slice(),
        _ => return Vec::new(),
    };
    rows.iter()
        .filter(|row| row.is_object())
        .map(item)
        .collect()
}

fn pick(row: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let mut value = row.get(*key);
        if let Some(nested) = value.and_then(Value::as_object) {
            value = nested
                .get("nickname")
                .or_else(|| nested.get("name"))
                .or_else(|| nested.get("username"));
        }
        if let Some(Value::String(text)) = value {
            if !text.is_empty() {
                return Some(text.clone());
            }
        }
        if let Some(Value::Number(number)) = value {
            return Some(number.to_string());
        }
    }
    None
}

fn item(row: &Value) -> Value {
    let published = match row.get("created_utc") {
        Some(Value::Number(_)) => unix_to_iso(row.get("created_utc")),
        _ => pick(row, &["published_at", "created_at", "time", "date"]),
    };
    make_item(
        pick(row, &["title", "name"]),
        pick(row, &["url", "link", "note_url"]),
        pick(row, &["author", "user", "nickname", "username"]),
        published,
        pick(row, &["content", "text", "desc", "description", "body"]),
        row.clone(),
    )
}

async fn site_items(
    site: &str,
    args: Vec<String>,
    timeout: Duration,
    state_dir: &Path,
    _login_hint: &str,
    empty_is_error: bool,
) -> Result<Vec<Value>> {
    let rows = call(site, &args, timeout, state_dir, site).await?;
    let items = items(&rows);
    if items.is_empty() {
        if empty_is_error {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                "OpenCLI finished but returned no items",
            ));
        }
        return Err(ReachError::new("NOT_FOUND", "nothing found"));
    }
    Ok(items)
}

fn take(mut items: Vec<Value>, limit: i64) -> Vec<Value> {
    items.truncate(limit.max(0) as usize);
    items
}

fn host(target: &str, domains: &[&str]) -> bool {
    crate::urlsec::host_matches(target, domains)
}

/// Execute one action of one OpenCLI site.
pub async fn execute(
    site: &str,
    action: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    match (site, action) {
        ("twitter", "search") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec!["search".into(), query, "--limit".into(), limit.to_string()];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("twitter", "profile") => {
            let username = req_str(params, "username")?
                .trim_start_matches('@')
                .to_string();
            let rows = site_items(
                site,
                vec!["profile".into(), username],
                timeout,
                state_dir,
                &login_hint(site),
                true,
            )
            .await?;
            Ok(take(rows, 1))
        }
        ("twitter", "tweets") => {
            let username = req_str(params, "username")?
                .trim_start_matches('@')
                .to_string();
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec![
                "tweets".into(),
                username,
                "--limit".into(),
                limit.to_string(),
            ];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("twitter", "thread") => {
            let target = req_str(params, "target")?;
            let id = target
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            site_items(
                site,
                vec!["thread".into(), id],
                timeout,
                state_dir,
                &login_hint(site),
                true,
            )
            .await
        }
        ("reddit", "search") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec!["search".into(), query, "--limit".into(), limit.to_string()];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("reddit", "read") => reddit_read(params, timeout, state_dir).await,
        ("reddit", "subreddit") => {
            let name = req_str(params, "name")?;
            let name = name.strip_prefix("r/").unwrap_or(&name).to_string();
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec![
                "subreddit".into(),
                name,
                "--limit".into(),
                limit.to_string(),
            ];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("xiaohongshu", "search") => xhs_search(params, timeout, state_dir).await,
        ("xiaohongshu", "read") => xhs_read(params, timeout, state_dir).await,
        ("xiaohongshu", "comments") => xhs_comments(params, timeout, state_dir).await,
        ("instagram", "search") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec!["search".into(), query, "--limit".into(), limit.to_string()];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("instagram", "profile") => {
            let username = req_str(params, "username")?;
            let rows = site_items(
                site,
                vec!["profile".into(), username],
                timeout,
                state_dir,
                &login_hint(site),
                false,
            )
            .await?;
            Ok(take(rows, 1))
        }
        ("instagram", "posts") => {
            let username = req_str(params, "username")?;
            let limit = int_param(params, "limit", 6, 0)?;
            let args = vec!["user".into(), username, "--limit".into(), limit.to_string()];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("facebook", "search") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let args = vec!["search".into(), query, "--limit".into(), limit.to_string()];
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("facebook", "profile") => {
            let username = req_str(params, "username")?;
            let rows = site_items(
                site,
                vec!["profile".into(), username],
                timeout,
                state_dir,
                &login_hint(site),
                false,
            )
            .await?;
            Ok(take(rows, 1))
        }
        ("linkedin", "jobs") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let mut args = vec!["search".into(), query, "--limit".into(), limit.to_string()];
            if let Some(location) = opt_str(params, "location") {
                args.push("--location".into());
                args.push(location);
            }
            Ok(take(
                site_items(site, args, timeout, state_dir, &login_hint(site), true).await?,
                limit,
            ))
        }
        ("linkedin", "read") => {
            let target = req_str(params, "target")?;
            let url = linkedin_job_url(&target)?;
            let rows = site_items(
                site,
                vec!["job-detail".into(), url],
                timeout,
                state_dir,
                &login_hint(site),
                true,
            )
            .await?;
            Ok(take(rows, 1))
        }
        ("boss", "search") => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 10, 0)?;
            let city = opt_str(params, "city").unwrap_or_else(|| "全国".to_string());
            let rows = call(
                site,
                &[
                    "search".into(),
                    query,
                    "--city".into(),
                    city,
                    "--limit".into(),
                    limit.to_string(),
                ],
                timeout,
                state_dir,
                site,
            )
            .await?;
            Ok(take(items(&rows), limit))
        }
        ("boss", "read") => {
            let target = req_str(params, "target")?;
            static JOB: LazyLock<regex::Regex> = LazyLock::new(|| {
                regex::Regex::new(r"/job_detail/([A-Za-z0-9_-]+)\.html").expect("boss job url")
            });
            if !host(&target, &["zhipin.com"]) {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    "Expected a direct zhipin.com job URL",
                ));
            }
            let job_id = JOB
                .captures(&target)
                .and_then(|captures| captures.get(1))
                .map(|found| found.as_str().to_string())
                .ok_or_else(|| ReachError::new("BAD_INPUT", "Expected a direct BOSS job URL"))?;
            let rows = match call(site, &["detail".into(), job_id], timeout, state_dir, site).await
            {
                Ok(rows) => rows,
                Err(error)
                    if error.code == "NEED_LOGIN"
                        && error
                            .message
                            .contains("BOSS redirected the job detail page to the login flow") =>
                {
                    return Err(ReachError::new(
                        "UPSTREAM_BROKEN",
                        "OpenCLI could not extract this BOSS JD; its login signal is ambiguous",
                    ));
                }
                Err(error) => return Err(error),
            };
            Ok(take(items(&rows), 1))
        }
        _ => Err(ReachError::new(
            "BAD_INPUT",
            format!("opencli has no mapping for {site}/{action}"),
        )),
    }
}

async fn reddit_read(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let target = req_str(params, "target")?;
    let text = target.trim();
    if !host(text, &["reddit.com", "redd.it"]) && !text.chars().all(char::is_alphanumeric) {
        return Err(ReachError::new(
            "BAD_INPUT",
            "target must be a reddit.com post link or post id",
        ));
    }
    let rows = site_items(
        "reddit",
        vec![
            "read".into(),
            text.to_string(),
            "--limit".into(),
            "20".into(),
        ],
        timeout,
        state_dir,
        &login_hint("reddit"),
        true,
    )
    .await?;
    let mut post = rows[0].clone();
    let comments = &rows[1..];
    if !comments.is_empty() {
        let mut lines = vec![post["text"].as_str().unwrap_or_default().to_string()];
        for comment in comments {
            lines.push(format!(
                "\n@{}: {}",
                comment["author"].as_str().unwrap_or_default(),
                comment["text"].as_str().unwrap_or_default()
            ));
        }
        post["text"] = json!(lines.join("\n").trim());
        post["raw"] = json!({
            "post": post["raw"].clone(),
            "comments": comments.iter().map(|comment| comment["raw"].clone()).collect::<Vec<_>>(),
        });
    }
    Ok(vec![post])
}

async fn xhs_search(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let query = req_str(params, "query")?;
    let limit = int_param(params, "limit", 5, 0)?;
    let rows = call(
        "xiaohongshu",
        &["search".into(), query, "--limit".into(), limit.to_string()],
        timeout,
        state_dir,
        "xiaohongshu",
    )
    .await?;
    let items = take(items(&rows), limit);
    if items.is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "OpenCLI finished but returned no notes",
        ));
    }
    Ok(items)
}

async fn xhs_read(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let url = req_str(params, "target")?;
    if !host(&url, &["xiaohongshu.com", "xhslink.com"]) {
        return Err(ReachError::new(
            "BAD_INPUT",
            "target must be a full Xiaohongshu note link including xsec_token",
        ));
    }
    let mut rows = call(
        "xiaohongshu",
        &["note".into(), url.clone()],
        timeout,
        state_dir,
        "xiaohongshu",
    )
    .await?;
    if let Some(data) = rows.get("data").and_then(Value::as_array) {
        rows = Value::Array(data.clone());
    }
    if let Value::Array(list) = &rows {
        let fields = !list.is_empty()
            && list
                .iter()
                .all(|row| row.get("field").is_some() && row.get("value").is_some());
        if fields {
            let mut note = serde_json::Map::new();
            for row in list {
                if let (Some(field), Some(value)) = (
                    row.get("field").and_then(Value::as_str),
                    row.get("value").cloned(),
                ) {
                    note.insert(field.to_string(), value);
                }
            }
            note.insert("url".into(), json!(url));
            rows = Value::Array(vec![Value::Object(note)]);
        }
    }
    let items = items(&rows);
    if items.is_empty() {
        return Err(ReachError::new("NOT_FOUND", "note not found"));
    }
    Ok(take(items, 1))
}

fn validate_comments(target: &str, limit: i64) -> Result<()> {
    static PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^/(?:explore|search_result)/[A-Za-z0-9_-]+$").expect("note path")
    });
    let parsed = url::Url::parse(target.trim()).map_err(|_| {
        ReachError::new("BAD_INPUT", "target must be a direct Xiaohongshu note URL")
    })?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some_and(|port| port != 443)
        || !host(target, &["xiaohongshu.com"])
        || !PATH.is_match(parsed.path())
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "target must be a direct Xiaohongshu note URL",
        ));
    }
    if !(1..=50).contains(&limit) {
        return Err(ReachError::new(
            "BAD_INPUT",
            "limit must be between 1 and 50 top-level comments",
        ));
    }
    Ok(())
}

async fn xhs_comments(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let target = req_str(params, "target")?;
    let limit = int_param(params, "limit", 20, 0)?;
    validate_comments(&target, limit)?;
    let rows = call(
        "xiaohongshu",
        &[
            "comments".into(),
            target.trim().to_string(),
            "--limit".into(),
            limit.to_string(),
            "--with-replies".into(),
            "true".into(),
        ],
        timeout,
        state_dir,
        "xiaohongshu",
    )
    .await?;
    let mut items = items(&rows);
    for item in &mut items {
        item["url"] = json!(target.trim());
        if let Some(raw) = item.get_mut("raw").and_then(Value::as_object_mut) {
            raw.insert("content_scope".into(), json!("loaded_comments"));
            raw.insert("exhaustive".into(), json!(false));
        }
    }
    Ok(items)
}

fn linkedin_job_url(target: &str) -> Result<String> {
    if !host(target, &["linkedin.com"]) {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Expected a linkedin.com job URL",
        ));
    }
    static JOB: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^/jobs/view/(?:[^/]+-)?([0-9]+)/?$").expect("linkedin job path")
    });
    let parsed = url::Url::parse(target.trim())
        .map_err(|_| ReachError::new("BAD_INPUT", "Expected a linkedin.com job URL"))?;
    let id = JOB
        .captures(parsed.path())
        .and_then(|captures| captures.get(1))
        .map(|found| found.as_str())
        .ok_or_else(|| {
            ReachError::new("BAD_INPUT", "Expected a LinkedIn /jobs/view/<job-id> URL")
        })?;
    Ok(format!("https://www.linkedin.com/jobs/view/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_list_is_zero_rows() {
        assert!(items(&json!({"ok": true, "data": []})).is_empty());
        assert!(items(&json!({"ok": true, "items": []})).is_empty());
    }

    #[test]
    fn a_single_object_is_one_row() {
        let rows = items(&json!({"ok": true, "data": {"title": "T"}}));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "T");
    }

    #[test]
    fn yaml_errors_preserve_upstream_help_and_codes() {
        let error = error_from_text(
            "ok: false\nerror:\n  code: RATE_LIMIT\n  message: slow down\n  help: retry later\n",
            None,
        );
        assert_eq!(error.code, "RATE_LIMITED");

        let error = error_from_text(
            "ok: false\nerror:\n  code: BROKEN\n  message: boom\n  help: retry later\n",
            None,
        );
        assert_eq!(error.code, "UPSTREAM_BROKEN");
        assert_eq!(error.hint, "retry later");

        let error = error_from_text(
            "ok: false\nerror:\n  code: AUTH_REQUIRED\n  message: log in\n  help: upstream\n",
            Some("arana login twitter"),
        );
        assert_eq!(error.code, "NEED_LOGIN");
        assert_eq!(error.hint, "arana login twitter");
    }
}
