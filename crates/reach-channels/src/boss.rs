//! `boss` channel backend through the pinned boss-agent-cli.
//!
//! The CLI is driven through its JSON envelope with an explicit existing
//! browser (`--browser-source existing-browser`): no independent HTTP session,
//! no stored passwords. Search references (`security_id`/`lid`) stay in a
//! private file under the session home and never reach the unified items.

use crate::params::{int_param, opt_str, req_str};
use crate::session;
use crate::tool;
use crate::Result;
use reach_core::{make_item, state::Store, ReachError};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The dedicated browser's CDP endpoint, overridable in state for a
/// non-default launch.
const DEFAULT_CDP: &str = "http://127.0.0.1:9222";
const BOSS_LOGIN_HINT: &str =
    "Run `arana login boss`; reuse the selected CDP browser and sync only after confirming login.";

fn cdp_endpoint(state_dir: &Path) -> String {
    Store::new(state_dir)
        .load()
        .ok()
        .and_then(|state| state["boss_cdp_url"].as_str().map(str::to_owned))
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CDP.to_string())
}

fn data_dir(state_dir: &Path) -> Result<PathBuf> {
    Ok(session::session_dir("boss-agent-cli", state_dir)?.join(".boss-agent"))
}

fn global_args(state_dir: &Path, cdp: &str) -> Result<Vec<String>> {
    Ok(vec![
        "--data-dir".into(),
        data_dir(state_dir)?.to_string_lossy().into_owned(),
        "--cdp-url".into(),
        cdp.to_string(),
        "--browser-source".into(),
        "existing-browser".into(),
        "--json".into(),
    ])
}

fn envelope_error(payload: &Value) -> ReachError {
    let error = payload.get("error").cloned().unwrap_or(Value::Null);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("boss-agent-cli failed")
        .to_string();
    let low = message.to_lowercase();
    if matches!(
        code.as_str(),
        "PLATFORM_BLOCKED" | "ACCOUNT_RISK" | "ENVIRONMENT_RISK" | "CAPTCHA_REQUIRED"
    ) || [
        "code 36",
        "code 37",
        "环境异常",
        "环境存在异常",
        "验证码",
        "安全验证",
        "风控",
    ]
    .iter()
    .any(|word| low.contains(word))
    {
        return ReachError::new("PLATFORM_BLOCKED", message);
    }
    if matches!(code.as_str(), "RATE_LIMITED" | "RATE_LIMIT") {
        return ReachError::new("RATE_LIMITED", message);
    }
    if code == "NEED_SETUP" {
        return ReachError::new("NEED_SETUP", message);
    }
    if code.contains("AUTH") || code.contains("LOGIN") {
        let mut error = ReachError::new("NEED_LOGIN", message);
        error.hint = BOSS_LOGIN_HINT.to_string();
        return error;
    }
    ReachError::new("UPSTREAM_BROKEN", message)
}

async fn run_json(state_dir: &Path, args: Vec<String>, timeout: Duration) -> Result<Value> {
    let program = tool::program("boss", "ARANA_BOSS_BIN", state_dir)?;
    let cdp = cdp_endpoint(state_dir);
    let mut argv = global_args(state_dir, &cdp)?;
    argv.extend(args);
    let env = session::session_env("boss-agent-cli", state_dir)?;
    let output = tool::run(&program, &argv, env, timeout, "boss").await?;
    let text = if output.stdout.trim().is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    if !output.success() && !output.stdout.contains('{') {
        return Err(tool::failed_exit(&output, "boss-agent-cli"));
    }
    let payload = tool::json_payload(text, "boss-agent-cli")?;
    if payload.get("ok") == Some(&Value::Bool(false)) {
        return Err(envelope_error(&payload));
    }
    Ok(payload)
}

fn strip_private(row: &Value) -> Value {
    let mut raw = row.as_object().cloned().unwrap_or_default();
    for key in ["security_id", "securityId", "lid"] {
        raw.remove(key);
    }
    Value::Object(raw)
}

fn job_item(row: &Value, text: Option<String>) -> Value {
    let job_id = row.get("job_id").and_then(Value::as_str);
    let url = job_id.map(|id| format!("https://www.zhipin.com/job_detail/{id}.html"));
    let text = text.unwrap_or_else(|| {
        row.get("description")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                ["salary", "city", "experience", "education"]
                    .iter()
                    .filter_map(|key| row.get(*key).and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
    });
    make_item(
        row.get("title").and_then(Value::as_str).map(str::to_string),
        url,
        row.get("company")
            .and_then(Value::as_str)
            .map(str::to_string),
        None,
        Some(text),
        strip_private(row),
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn refs_path(state_dir: &Path) -> Result<PathBuf> {
    Ok(data_dir(state_dir)?.join("reach-job-refs.json"))
}

fn save_refs(state_dir: &Path, rows: &[Value]) -> Result<()> {
    let path = refs_path(state_dir)?;
    let parent = path.parent().expect("refs parent");
    std::fs::create_dir_all(parent)?;
    if std::fs::symlink_metadata(&path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Unmanaged job-reference path; left untouched",
        ));
    }
    let mut jobs = Map::new();
    for row in rows {
        let job_id = row.get("job_id").and_then(Value::as_str);
        let security_id = row.get("security_id").and_then(Value::as_str);
        if let (Some(job_id), Some(security_id)) = (job_id, security_id) {
            jobs.insert(
                job_id.to_string(),
                json!({
                    "security_id": security_id,
                    "lid": row.get("lid").and_then(Value::as_str).unwrap_or_default(),
                }),
            );
        }
    }
    let body = json!({"at": now(), "jobs": jobs});
    let bytes = serde_json::to_vec(&body)
        .map_err(|error| ReachError::new("UPSTREAM_BROKEN", error.to_string()))?;
    std::fs::write(&path, bytes)?;
    Ok(())
}

fn recent_reference(state_dir: &Path, job_id: &str) -> Result<(String, String)> {
    let path = refs_path(state_dir)?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        ReachError::new(
            "NEED_SETUP",
            "No recent search reference; use the job link returned by a fresh search",
        )
    })?;
    let saved: Value = serde_json::from_str(&text).map_err(|_| {
        ReachError::new(
            "NEED_SETUP",
            "No recent search reference; use the job link returned by a fresh search",
        )
    })?;
    let fresh = saved
        .get("at")
        .and_then(Value::as_i64)
        .is_some_and(|at| now() - at <= 3600);
    let reference = saved
        .get("jobs")
        .and_then(|jobs| jobs.get(job_id))
        .filter(|_| fresh)
        .ok_or_else(|| {
            ReachError::new(
                "NEED_SETUP",
                "No recent search reference; use the job link returned by a fresh search",
            )
        })?;
    let security_id = reference
        .get("security_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ReachError::new(
                "NEED_SETUP",
                "No recent search reference; use the job link returned by a fresh search",
            )
        })?;
    let lid = reference
        .get("lid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((security_id.to_string(), lid))
}

/// Execute `boss-agent-cli` for `search` and `read`.
pub async fn cli_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    match action {
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 10, 0)?;
            let mut args = vec!["search".into(), query];
            if let Some(city) = opt_str(params, "city") {
                args.push("--city".into());
                args.push(city);
            }
            args.push("--page".into());
            args.push("1".into());
            let payload = run_json(state_dir, args, timeout).await?;
            let rows = data_rows(&payload);
            save_refs(state_dir, &rows)?;
            Ok(rows
                .iter()
                .take(limit.max(0) as usize)
                .map(|row| job_item(row, None))
                .collect())
        }
        "read" => {
            let target = req_str(params, "target")?;
            static JOB: LazyLock<regex::Regex> = LazyLock::new(|| {
                regex::Regex::new(r"/job_detail/([A-Za-z0-9_-]+)\.html").expect("boss job url")
            });
            if !crate::urlsec::host_matches(&target, &["zhipin.com"]) {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    "Expected a direct BOSS job URL",
                ));
            }
            let job_id = JOB
                .captures(&target)
                .and_then(|captures| captures.get(1))
                .map(|found| found.as_str().to_string())
                .ok_or_else(|| ReachError::new("BAD_INPUT", "Expected a direct BOSS job URL"))?;
            let (security_id, lid) = recent_reference(state_dir, &job_id)?;
            let mut args = vec!["detail".into(), security_id];
            if !lid.is_empty() {
                args.push("--lid".into());
                args.push(lid);
            }
            let payload = run_json(state_dir, args, timeout).await?;
            let data = payload.get("data").cloned().unwrap_or(Value::Null);
            let rows = match data {
                Value::Array(rows) => rows,
                Value::Object(row) => vec![Value::Object(row)],
                _ => Vec::new(),
            };
            Ok(rows.iter().take(1).map(|row| job_item(row, None)).collect())
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("boss-agent-cli has no action {other}"),
        )),
    }
}

fn data_rows(payload: &Value) -> Vec<Value> {
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    match data {
        Value::Array(rows) => rows,
        Value::Object(map) => ["items", "jobs"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_array).cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
