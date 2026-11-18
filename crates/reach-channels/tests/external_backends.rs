//! External backend adapters (OpenCLI, bili-cli, boss-agent-cli) against
//! executable fixtures: exact argv, private session env, output mapping and
//! the YAML/JSON failure taxonomy, with no live tool or network dependency.

// The env guard is intentionally held across `.await`: the child must inherit
// the overrides for the entire call, and other tests must not mutate PATH-like
// variables meanwhile.
#![allow(clippy::await_holding_lock)]

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// Environment overrides are process-wide: every test that sets or depends on
/// one holds this guard (Windows Rust testing rules).
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

struct EnvVar {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVar {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn script_body() -> String {
    if cfg!(windows) {
        "@echo off\r\n\
             echo ARGS %* >\"%~dp0record.txt\"\r\n\
             echo HOME [%HOME%]>>\"%~dp0record.txt\"\r\n\
             echo USERPROFILE [%USERPROFILE%]>>\"%~dp0record.txt\"\r\n\
             echo XDG [%XDG_CONFIG_HOME%]>>\"%~dp0record.txt\"\r\n\
             echo APPDATA [%APPDATA%]>>\"%~dp0record.txt\"\r\n\
             echo LOCALAPPDATA [%LOCALAPPDATA%]>>\"%~dp0record.txt\"\r\n\
             echo PROFILE [%OPENCLI_PROFILE%]>>\"%~dp0record.txt\"\r\n\
             echo WINDOW [%OPENCLI_WINDOW%]>>\"%~dp0record.txt\"\r\n\
             echo CDP [%OPENCLI_CDP_ENDPOINT%]>>\"%~dp0record.txt\"\r\n\
             echo TARGET [%OPENCLI_CDP_TARGET%]>>\"%~dp0record.txt\"\r\n\
             type \"%~dp0response.json\"\r\n"
            .to_string()
    } else {
        "#!/bin/sh\nd=$(dirname \"$0\")\n{\n  printf 'ARGS %s\\n' \"$*\"\n  printf 'HOME [%s]\\n' \"$HOME\"\n  printf 'USERPROFILE [%s]\\n' \"$USERPROFILE\"\n  printf 'XDG [%s]\\n' \"$XDG_CONFIG_HOME\"\n  printf 'APPDATA [%s]\\n' \"$APPDATA\"\n  printf 'LOCALAPPDATA [%s]\\n' \"$LOCALAPPDATA\"\n  printf 'PROFILE [%s]\\n' \"$OPENCLI_PROFILE\"\n  printf 'WINDOW [%s]\\n' \"$OPENCLI_WINDOW\"\n  printf 'CDP [%s]\\n' \"$OPENCLI_CDP_ENDPOINT\"\n  printf 'TARGET [%s]\\n' \"$OPENCLI_CDP_TARGET\"\n} > \"$d/record.txt\"\ncat \"$d/response.json\"\n"
            .to_string()
    }
}

/// A fake external tool: records argv/env, prints a canned response.
struct Tool {
    _dir: tempfile::TempDir,
    binary: PathBuf,
    response: PathBuf,
}

impl Tool {
    fn new(name: &str, text: &str) -> Self {
        let dir = tempfile::tempdir().expect("tool dir");
        let script = dir.path().join(if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_string()
        });
        std::fs::write(&script, script_body()).expect("write fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fixture");
        }
        let response = dir.path().join("response.json");
        std::fs::write(&response, text).expect("write response");
        Self {
            _dir: dir,
            binary: script,
            response,
        }
    }

    fn respond(&self, text: &str) {
        std::fs::write(&self.response, text).expect("update response");
    }

    fn record(&self) -> std::collections::HashMap<String, String> {
        let path = self.binary.parent().unwrap().join("record.txt");
        std::fs::read_to_string(path)
            .expect("fixture record")
            .lines()
            .map(|line| {
                let line = line.trim_end_matches('\r');
                match line.split_once(' ') {
                    Some((key, value)) => (key.to_string(), value.trim().to_string()),
                    None => (line.to_string(), String::new()),
                }
            })
            .collect()
    }
}

/// Split a recorded command line into argument tokens. Shells re-quote an
/// argument when they echo `%*` (the Windows runner's short temp path is one
/// case), so compare tokens rather than raw text.
fn argv(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn state_with_context(context: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("state dir");
    let mut state = json!({"version": 1, "tools": {}, "owned_paths": {}});
    if let Some(context) = context {
        state["opencli_context"] = json!(context);
    }
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .expect("state");
    dir
}

async fn call(
    channel: &str,
    action: &str,
    backend: &str,
    params: Value,
    state: &Path,
) -> reach_core::Result<Vec<Value>> {
    reach_channels::execute_with(
        channel,
        action,
        backend,
        &params,
        Duration::from_secs(30),
        state,
        &reach_channels::http::Endpoints::default(),
    )
    .await
}

fn opencli_response() -> String {
    json!({
        "ok": true,
        "data": [
            {"title": "A tweet", "url": "https://x.com/openai/status/1",
             "author": {"nickname": "OpenAI"}, "created_utc": 1_700_000_000,
             "content": "hello", "id": "1"}
        ]
    })
    .to_string()
}

#[tokio::test]
async fn opencli_twitter_search_passes_argv_env_and_maps_items() {
    let _guard = env_lock();
    let tool = Tool::new("opencli", &opencli_response());
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-42"));

    let items = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 2}),
        state.path(),
    )
    .await
    .expect("twitter search");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "A tweet");
    assert_eq!(items[0]["author"], "OpenAI");
    assert_eq!(items[0]["published_at"], "2023-11-14T22:13:20Z");
    assert_eq!(items[0]["text"], "hello");
    assert_eq!(items[0]["raw"]["id"], "1");

    let record = tool.record();
    assert_eq!(record["ARGS"], "twitter search openai --limit 2 -f json");
    assert_eq!(record["PROFILE"], "[ctx-42]");
    assert_eq!(record["WINDOW"], "[background]");
    assert_eq!(record["CDP"], "[]");
    assert_eq!(record["TARGET"], "[]");
}

#[tokio::test]
async fn opencli_empty_data_is_zero_items_and_unbound_profile_is_need_setup() {
    let _guard = env_lock();
    let tool = Tool::new("opencli", &json!({"ok": true, "data": []}).to_string());
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-1"));

    let items = call(
        "boss",
        "search",
        "opencli",
        json!({"query": "rust", "limit": 3}),
        state.path(),
    )
    .await
    .expect("boss opencli search");
    assert!(items.is_empty(), "{{ok:true,data:[]}} must mean zero items");

    let unbound = state_with_context(None);
    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 1}),
        unbound.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_SETUP");
    assert!(error.message.contains("dedicated browser"));
}

#[tokio::test]
async fn opencli_yaml_errors_use_the_unified_taxonomy() {
    let _guard = env_lock();
    let tool = Tool::new(
        "opencli",
        "ok: false\nerror:\n  code: AUTH_REQUIRED\n  message: not signed in\n  help: log in yourself\n",
    );
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-1"));

    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");
    assert!(error.message.contains("not signed in"));
    assert!(error.hint.contains("arana login twitter"));

    tool.respond(
        "node: warning before the envelope\nok: false\nerror:\n  code: RATE_LIMITED\n  message: HTTP 429 too many requests\n",
    );
    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "RATE_LIMITED");

    tool.respond("ok: false\nerror:\n  code: UNKNOWN\n  message: 环境存在异常\n");
    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "PLATFORM_BLOCKED");

    tool.respond("");
    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "openai", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
}

#[tokio::test]
async fn xiaohongshu_read_unwraps_field_rows_and_validates_targets() {
    let _guard = env_lock();
    let rows = json!({
        "ok": true,
        "data": [
            {"field": "title", "value": "Note title"},
            {"field": "content", "value": "Note body"},
            {"field": "author", "value": "someone"}
        ]
    })
    .to_string();
    let tool = Tool::new("opencli", &rows);
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-1"));
    let target = "https://www.xiaohongshu.com/explore/abc123";

    let items = call(
        "xiaohongshu",
        "read",
        "opencli",
        json!({"target": target}),
        state.path(),
    )
    .await
    .expect("xhs read");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Note title");
    assert_eq!(items[0]["url"], target);
    assert_eq!(items[0]["text"], "Note body");
    assert_eq!(
        tool.record()["ARGS"],
        format!("xiaohongshu note {target} -f json")
    );

    let error = call(
        "xiaohongshu",
        "comments",
        "opencli",
        json!({"target": "https://www.xiaohongshu.com/explore/abc123", "limit": 51}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
    assert!(error.message.contains("between 1 and 50"));

    let error = call(
        "xiaohongshu",
        "read",
        "opencli",
        json!({"target": "https://example.com/explore/abc"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
}

#[tokio::test]
async fn reddit_read_merges_post_and_comments() {
    let _guard = env_lock();
    let rows = json!([
        {"title": "Post", "url": "https://reddit.com/r/python/1", "author": "op", "text": "body"},
        {"author": "a", "text": "first"},
        {"author": "b", "text": "second"}
    ])
    .to_string();
    let tool = Tool::new("opencli", &rows);
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-1"));

    let items = call(
        "reddit",
        "read",
        "opencli",
        json!({"target": "https://www.reddit.com/r/python/comments/1/post/"}),
        state.path(),
    )
    .await
    .expect("reddit read");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Post");
    assert_eq!(items[0]["text"], "body\n\n@a: first\n\n@b: second");
    assert_eq!(items[0]["raw"]["comments"].as_array().unwrap().len(), 2);
    assert_eq!(
        tool.record()["ARGS"],
        "reddit read https://www.reddit.com/r/python/comments/1/post/ --limit 20 -f json"
    );

    let error = call(
        "reddit",
        "read",
        "opencli",
        json!({"target": "https://example.com/post"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
}

#[tokio::test]
async fn bili_cli_search_uses_private_home_and_maps_items() {
    let _guard = env_lock();
    let rows = json!({
        "ok": true,
        "data": [
            {"id": "BV1cBsA63EKu", "bvid": "BV1cBsA63EKu", "title": "Rust",
             "author": "up", "play": 1, "duration": "8:37"}
        ]
    })
    .to_string();
    let tool = Tool::new("bili", &rows);
    let _env = EnvVar::set("ARANA_BILI_BIN", &tool.binary);
    let state = state_with_context(None);

    let items = call(
        "bilibili",
        "search",
        "bili-cli",
        json!({"query": "rust", "limit": 2}),
        state.path(),
    )
    .await
    .expect("bili search");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust");
    assert_eq!(items[0]["author"], "up");
    assert_eq!(
        items[0]["url"],
        "https://www.bilibili.com/video/BV1cBsA63EKu"
    );
    assert_eq!(items[0]["raw"]["play"], 1);

    let record = tool.record();
    assert_eq!(record["ARGS"], "search rust --type video -n 2 --json");
    let home = state.path().join("sessions").join("bili-cli");
    assert_eq!(record["HOME"], format!("[{}]", home.display()));
    assert_eq!(record["USERPROFILE"], format!("[{}]", home.display()));
    assert_eq!(record["XDG"], format!("[{}]", home.display()));
    assert_eq!(record["APPDATA"], format!("[{}]", home.display()));
    assert_eq!(record["LOCALAPPDATA"], format!("[{}]", home.display()));

    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(state.path().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(ledger["owned_paths"]["sessions/bili-cli"]["owner"], "arana");
    assert_eq!(
        ledger["owned_paths"]["sessions/bili-cli"]["scope"],
        "session"
    );
    assert_eq!(
        ledger["owned_paths"]["sessions/bili-cli"]["kind"],
        "directory"
    );
}

#[tokio::test]
async fn bili_cli_errors_and_unavailable_subtitles_keep_the_taxonomy() {
    let _guard = env_lock();
    let tool = Tool::new("bili", &json!({"ok": true, "data": []}).to_string());
    let _env = EnvVar::set("ARANA_BILI_BIN", &tool.binary);
    let state = state_with_context(None);

    tool.respond(
        &json!({"ok": false, "error": {"code": "not_found", "message": "gone"}}).to_string(),
    );
    let error = call(
        "bilibili",
        "read",
        "bili-cli",
        json!({"target": "BV1GJ411x7h7"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NOT_FOUND");

    tool.respond(
        &json!({"ok": false, "error": {"code": "auth_required", "message": "please login"}})
            .to_string(),
    );
    let error = call(
        "bilibili",
        "subtitle",
        "bili-cli",
        json!({"target": "BV1GJ411x7h7"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");
    assert!(error.hint.contains("QR"));

    tool.respond(
        &json!({
            "ok": true,
            "data": {
                "video": {"bvid": "BV1GJ411x7h7", "title": "V", "owner": {"name": "up"}},
                "subtitle": {"available": false},
                "warnings": [{"code": "subtitle_unavailable"}]
            }
        })
        .to_string(),
    );
    let error = call(
        "bilibili",
        "subtitle",
        "bili-cli",
        json!({"target": "BV1GJ411x7h7"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");

    let credential = state
        .path()
        .join("sessions")
        .join("bili-cli")
        .join(".bilibili-cli")
        .join("credential.json");
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, r#"{"sessdata":""}"#).unwrap();
    let error = call(
        "bilibili",
        "subtitle",
        "bili-cli",
        json!({"target": "BV1GJ411x7h7"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN", "empty sessdata is not a login");

    std::fs::write(&credential, r#"{"sessdata": "token"}"#).unwrap();
    let error = call(
        "bilibili",
        "subtitle",
        "bili-cli",
        json!({"target": "BV1GJ411x7h7"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
}

#[tokio::test]
async fn boss_cli_search_strips_private_ids_and_read_uses_the_saved_reference() {
    let _guard = env_lock();
    let rows = json!({
        "ok": true,
        "command": "search",
        "data": [
            {"job_id": "enc1", "title": "Rust Engineer", "company": "ACME",
             "salary": "20-30K", "city": "深圳", "experience": "3-5年",
             "education": "本科", "description": "Build things",
             "security_id": "SECRET", "lid": "LID"}
        ],
        "pagination": {"page": 1}
    })
    .to_string();
    let tool = Tool::new("boss", &rows);
    let _env = EnvVar::set("ARANA_BOSS_BIN", &tool.binary);
    let state = state_with_context(None);

    let items = call(
        "boss",
        "search",
        "boss-agent-cli",
        json!({"query": "rust", "city": "Shenzhen", "limit": 5}),
        state.path(),
    )
    .await
    .expect("boss search");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Rust Engineer");
    assert_eq!(items[0]["author"], "ACME");
    assert_eq!(items[0]["text"], "Build things");
    assert_eq!(
        items[0]["url"],
        "https://www.zhipin.com/job_detail/enc1.html"
    );
    assert!(items[0]["raw"].get("security_id").is_none());
    assert!(items[0]["raw"].get("lid").is_none());

    let record = tool.record();
    let data_dir = state
        .path()
        .join("sessions")
        .join("boss-agent-cli")
        .join(".boss-agent");
    let expected = vec![
        "--data-dir".to_string(),
        data_dir.display().to_string(),
        "--cdp-url".to_string(),
        "http://127.0.0.1:9222".to_string(),
        "--browser-source".to_string(),
        "existing-browser".to_string(),
        "--json".to_string(),
        "search".to_string(),
        "rust".to_string(),
        "--city".to_string(),
        "Shenzhen".to_string(),
        "--page".to_string(),
        "1".to_string(),
    ];
    assert_eq!(argv(&record["ARGS"]), expected);
    assert_eq!(
        record["HOME"],
        format!(
            "[{}]",
            state
                .path()
                .join("sessions")
                .join("boss-agent-cli")
                .display()
        )
    );

    tool.respond(
        &json!({
            "ok": true,
            "command": "detail",
            "data": {"job_id": "enc1", "title": "Rust Engineer", "company": "ACME",
                     "salary": "20-30K", "city": "深圳", "experience": "3-5年",
                     "education": "本科", "description": "Full JD", "security_id": "SECRET"}
        })
        .to_string(),
    );
    let items = call(
        "boss",
        "read",
        "boss-agent-cli",
        json!({"target": "https://www.zhipin.com/job_detail/enc1.html"}),
        state.path(),
    )
    .await
    .expect("boss read");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "Full JD");
    assert!(items[0]["raw"].get("security_id").is_none());
    assert_eq!(
        argv(&tool.record()["ARGS"]),
        vec![
            "--data-dir".to_string(),
            data_dir.display().to_string(),
            "--cdp-url".to_string(),
            "http://127.0.0.1:9222".to_string(),
            "--browser-source".to_string(),
            "existing-browser".to_string(),
            "--json".to_string(),
            "detail".to_string(),
            "SECRET".to_string(),
            "--lid".to_string(),
            "LID".to_string(),
        ]
    );

    let fresh = state_with_context(None);
    let error = call(
        "boss",
        "read",
        "boss-agent-cli",
        json!({"target": "https://www.zhipin.com/job_detail/enc1.html"}),
        fresh.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_SETUP");
    assert!(error.message.contains("recent search reference"));
}

#[tokio::test]
async fn boss_cli_risk_and_auth_errors_are_final_codes() {
    let _guard = env_lock();
    let tool = Tool::new(
        "boss",
        &json!({"ok": false, "error": {"code": "ENVIRONMENT_RISK", "message": "环境异常"}})
            .to_string(),
    );
    let _env = EnvVar::set("ARANA_BOSS_BIN", &tool.binary);
    let state = state_with_context(None);

    let error = call(
        "boss",
        "search",
        "boss-agent-cli",
        json!({"query": "rust", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "PLATFORM_BLOCKED");

    tool.respond(
        &json!({"ok": false, "error": {"code": "AUTH_REQUIRED", "message": "未登录"}}).to_string(),
    );
    let error = call(
        "boss",
        "search",
        "boss-agent-cli",
        json!({"query": "rust", "limit": 1}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");
    assert!(error.hint.contains("login boss"));
}

#[tokio::test]
async fn boss_opencli_read_rejects_ambiguous_login_and_bad_urls() {
    let _guard = env_lock();
    let tool = Tool::new(
        "opencli",
        "ok: false\nerror:\n  code: NEED_LOGIN\n  message: BOSS redirected the job detail page to the login flow\n",
    );
    let _env = EnvVar::set("ARANA_OPENCLI_BIN", &tool.binary);
    let state = state_with_context(Some("ctx-1"));

    let error = call(
        "boss",
        "read",
        "opencli",
        json!({"target": "https://www.zhipin.com/job_detail/abc.html"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
    assert!(error.message.contains("ambiguous"));

    let error = call(
        "boss",
        "read",
        "opencli",
        json!({"target": "https://example.com/job_detail/abc.html"}),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
}

/// Credential-free live probe of the real bili-cli adapter. Run explicitly:
/// `cargo test -p reach-channels --test external_backends -- --ignored`.
#[tokio::test]
#[ignore = "live: bili-cli search/read against bilibili"]
async fn bili_cli_live_search_read_and_subtitle_taxonomy() {
    let state = state_with_context(None);
    let items = call(
        "bilibili",
        "search",
        "bili-cli",
        json!({"query": "Rust", "limit": 2}),
        state.path(),
    )
    .await
    .expect("live bili-cli search");
    assert!(!items.is_empty(), "live search returned no items");
    assert!(items[0]["url"]
        .as_str()
        .is_some_and(|url| url.starts_with("https://www.bilibili.com/video/BV")));
    let bvid = items[0]["raw"]["bvid"]
        .as_str()
        .expect("live search row bvid")
        .to_string();

    let read = call(
        "bilibili",
        "read",
        "bili-cli",
        json!({"target": bvid}),
        state.path(),
    )
    .await
    .expect("live bili-cli read");
    assert_eq!(read.len(), 1);
    assert!(read[0]["title"].is_string());

    let subtitle = call(
        "bilibili",
        "subtitle",
        "bili-cli",
        json!({"target": bvid}),
        state.path(),
    )
    .await;
    match subtitle {
        Ok(items) => {
            assert_eq!(items.len(), 1);
            println!("live subtitle ok");
        }
        Err(error) => {
            assert!(
                matches!(
                    error.code.as_str(),
                    "NEED_LOGIN" | "NOT_FOUND" | "UPSTREAM_BROKEN"
                ),
                "unexpected subtitle error: {error}"
            );
            println!(
                "live subtitle blocked honestly: {} {}",
                error.code, error.message
            );
        }
    }
}
