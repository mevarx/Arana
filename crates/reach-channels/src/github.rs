//! `github` channel: the `gh` CLI first, then the public REST API.
//!
//! The REST backend works without any token (60 requests/hour); a token from
//! `GH_TOKEN`/`GITHUB_TOKEN` or `config.yaml github_token` lifts the limit.

use crate::http::{self, Endpoints};
use crate::params::{int_param, req_str};
use crate::{Result, Value};
use reach_core::{make_item, ReachError};
use serde_json::{json, Map, Value as Json};
use std::path::Path;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

const README_MAX_CHARS: usize = 50_000;
const API_ACCEPT: &str = "application/vnd.github+json";

/// `owner/repo`, a repo link, or an issue/PR link -> (owner, repo, number).
pub fn parse_target(target: &str) -> Result<(String, String, Option<i64>)> {
    static TARGET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"^(?:https?://(?:www\.)?github\.com/)?([\w.-]+)/([\w.-]+?)(?:\.git)?(?:/(?:issues|pull)/(\d+))?/?(?:[?#].*)?$",
        )
        .expect("github target regex")
    });
    let text = target.trim();
    let captures = TARGET.captures(text).ok_or_else(|| {
        ReachError::new(
            "BAD_INPUT",
            "target must be owner/repo or a github.com repo/issue/PR link",
        )
    })?;
    let owner = captures[1].to_string();
    let repo = captures[2].to_string();
    let number = captures.get(3).and_then(|m| m.as_str().parse::<i64>().ok());
    Ok((owner, repo, number))
}

fn encode(value: &str) -> String {
    crate::params::encode_component(value)
}

/// Response of one API/CLI `get(path)` call: raw bytes.
enum Getter<'a> {
    Gh,
    Api(&'a Endpoints, Option<String>),
}

fn repo_item(repo: &Json, readme: Option<String>) -> Value {
    let mut raw = Map::new();
    for key in [
        "description",
        "stargazers_count",
        "forks_count",
        "language",
        "topics",
        "pushed_at",
        "open_issues_count",
        "archived",
    ] {
        raw.insert(
            key.to_string(),
            repo.get(key).cloned().unwrap_or(Json::Null),
        );
    }
    raw.insert(
        "license".into(),
        repo.get("license")
            .and_then(|license| license.get("spdx_id"))
            .cloned()
            .unwrap_or(Json::Null),
    );
    make_item(
        repo.get("full_name")
            .and_then(Json::as_str)
            .map(str::to_string),
        repo.get("html_url")
            .and_then(Json::as_str)
            .map(str::to_string),
        repo.get("owner")
            .and_then(|owner| owner.get("login"))
            .and_then(Json::as_str)
            .map(str::to_string),
        repo.get("created_at")
            .and_then(Json::as_str)
            .map(str::to_string),
        readme.or_else(|| {
            repo.get("description")
                .and_then(Json::as_str)
                .map(str::to_string)
        }),
        Json::Object(raw),
    )
}

/// Token from the environment or `config.yaml` in the state directory.
fn config_token(state_dir: Option<&Path>) -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    let path = state_dir?.join("config.yaml");
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let token = parsed
        .get("github_token")
        .and_then(|value| value.as_str())?;
    if token.trim().is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

async fn api_get(
    endpoints: &Endpoints,
    path: &str,
    timeout: Duration,
    accept: &str,
    token: Option<&str>,
) -> Result<String> {
    let url = format!("{}{}", endpoints.github_api.trim_end_matches('/'), path);
    let mut headers = vec![("Accept", accept), ("User-Agent", "agent-reach")];
    let bearer;
    if let Some(token) = token {
        bearer = format!("Bearer {token}");
        headers.push(("Authorization", bearer.as_str()));
    }
    let response = http::get_raw(&url, timeout, &headers, http::DEFAULT_MAX_BYTES).await?;
    match response.status {
        200..=299 => Ok(response.body),
        404 => Err(ReachError::new("NOT_FOUND", "GitHub API returned HTTP 404")),
        429 => Err(rate_limited()),
        403 => {
            let exhausted = response
                .rate_limit_remaining
                .as_deref()
                .map(|value| value.trim() == "0")
                .unwrap_or(false);
            if exhausted || response.retry_after.is_some() {
                Err(rate_limited())
            } else {
                Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    "GitHub API denied this request (HTTP 403); no rate-limit evidence",
                ))
            }
        }
        status => Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("GitHub API returned HTTP {status}"),
        )),
    }
}

fn rate_limited() -> ReachError {
    ReachError::new(
        "RATE_LIMITED",
        "GitHub API rate limit (60/hour without a token)",
    )
}

async fn gh_get(path: &str, timeout: Duration, accept: &str) -> Result<String> {
    let executable =
        which::which("gh").map_err(|_| ReachError::new("NEED_SETUP", "gh is not installed"))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("api")
        .arg("-H")
        .arg(format!("Accept: {accept}"))
        .arg(path)
        .env("GH_TELEMETRY", "false")
        .env("DO_NOT_TRACK", "true")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x0800_0000);
    }
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ReachError::new("NEED_SETUP", "gh is not installed")
        } else {
            ReachError::new("UPSTREAM_BROKEN", error.to_string())
        }
    })?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(ReachError::new("UPSTREAM_BROKEN", error.to_string())),
        Err(_) => {
            return Err(ReachError::new(
                "TIMEOUT",
                format!("gh did not finish within {:.0}s", timeout.as_secs_f64()),
            ))
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    if stderr.contains("http 404") || stderr.contains("not found") {
        return Err(ReachError::new("NOT_FOUND", "gh api returned 404"));
    }
    if stderr.contains("gh auth login") || stderr.contains("authentication") {
        return Err(ReachError::new(
            "NEED_SETUP",
            "gh has no usable credentials; try the public GitHub API",
        ));
    }
    if stderr.contains("rate limit") {
        return Err(rate_limited());
    }
    let tail = String::from_utf8_lossy(&output.stderr)
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("no error output")
        .to_string();
    Err(ReachError::new(
        "UPSTREAM_BROKEN",
        format!("gh exited {}: {tail}", output.status.code().unwrap_or(-1)),
    ))
}

async fn get(getter: &Getter<'_>, path: &str, timeout: Duration, accept: &str) -> Result<String> {
    match getter {
        Getter::Gh => gh_get(path, timeout, accept).await,
        Getter::Api(endpoints, token) => {
            api_get(endpoints, path, timeout, accept, token.as_deref()).await
        }
    }
}

async fn search(
    getter: &Getter<'_>,
    query: &str,
    limit: i64,
    timeout: Duration,
) -> Result<Vec<Value>> {
    let path = format!(
        "/search/repositories?q={}&per_page={}",
        encode(query),
        limit.max(1)
    );
    let body = get(getter, &path, timeout, API_ACCEPT).await?;
    let data: Json = serde_json::from_str(&body).map_err(|_| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "GitHub returned an unreadable search response",
        )
    })?;
    let items = data
        .get("items")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .take(limit.max(0) as usize)
        .map(|repo| repo_item(repo, None))
        .collect())
}

async fn read_repo(getter: &Getter<'_>, target: &str, timeout: Duration) -> Result<Vec<Value>> {
    let (owner, repo, number) = parse_target(target)?;
    if number.is_none() {
        let info_body = get(
            getter,
            &format!("/repos/{owner}/{repo}"),
            timeout / 2,
            API_ACCEPT,
        )
        .await?;
        let info: Json = serde_json::from_str(&info_body).map_err(|_| {
            ReachError::new(
                "UPSTREAM_BROKEN",
                "GitHub returned an unreadable repository response",
            )
        })?;
        let readme = match get(
            getter,
            &format!("/repos/{owner}/{repo}/readme"),
            timeout / 2,
            "application/vnd.github.raw",
        )
        .await
        {
            Ok(text) => Some(text.chars().take(README_MAX_CHARS).collect::<String>()),
            Err(error) if error.code == "NOT_FOUND" => None,
            Err(error) => return Err(error),
        };
        return Ok(vec![repo_item(&info, readme)]);
    }
    let number = number.unwrap_or_default();
    let issue_body = get(
        getter,
        &format!("/repos/{owner}/{repo}/issues/{number}"),
        timeout / 2,
        API_ACCEPT,
    )
    .await?;
    let issue: Json = serde_json::from_str(&issue_body).map_err(|_| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "GitHub returned an unreadable issue response",
        )
    })?;
    let comments_body = get(
        getter,
        &format!("/repos/{owner}/{repo}/issues/{number}/comments?per_page=50"),
        timeout / 2,
        API_ACCEPT,
    )
    .await?;
    let comments: Vec<Json> = serde_json::from_str(&comments_body).unwrap_or_default();
    let mut parts = vec![issue
        .get("body")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string()];
    for comment in &comments {
        let who = comment
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(Json::as_str)
            .unwrap_or_default();
        let body = comment
            .get("body")
            .and_then(Json::as_str)
            .unwrap_or_default();
        parts.push(format!("\n@{who}: {body}"));
    }
    let labels: Vec<String> = issue
        .get("labels")
        .and_then(Json::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.get("name").and_then(Json::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(vec![make_item(
        issue
            .get("title")
            .and_then(Json::as_str)
            .map(str::to_string),
        issue
            .get("html_url")
            .and_then(Json::as_str)
            .map(str::to_string),
        issue
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(Json::as_str)
            .map(str::to_string),
        issue
            .get("created_at")
            .and_then(Json::as_str)
            .map(str::to_string),
        Some(parts.join("\n").trim().to_string()),
        json!({
            "state": issue.get("state"),
            "labels": labels,
            "comments": issue.get("comments"),
            "is_pull_request": issue.get("pull_request").is_some(),
        }),
    )])
}

/// `gh` CLI backend.
pub async fn gh_backend(action: &str, params: &Value, timeout: Duration) -> Result<Vec<Value>> {
    let getter = Getter::Gh;
    match action {
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 1)?;
            search(&getter, &query, limit, timeout).await
        }
        "read" => read_repo(&getter, &req_str(params, "target")?, timeout).await,
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("github has no action {other}"),
        )),
    }
}

/// Public REST backend (no token required); a configured token lifts the limit.
pub async fn api_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    let token = config_token(Some(state_dir));
    let getter = Getter::Api(endpoints, token);
    match action {
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 1)?;
            search(&getter, &query, limit, timeout).await
        }
        "read" => read_repo(&getter, &req_str(params, "target")?, timeout).await,
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("github has no action {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repos_and_issue_links() {
        assert_eq!(
            parse_target("yt-dlp/yt-dlp").unwrap(),
            ("yt-dlp".into(), "yt-dlp".into(), None)
        );
        assert_eq!(
            parse_target("https://github.com/yt-dlp/yt-dlp").unwrap(),
            ("yt-dlp".into(), "yt-dlp".into(), None)
        );
        assert_eq!(
            parse_target("https://github.com/owner/repo/issues/42").unwrap(),
            ("owner".into(), "repo".into(), Some(42))
        );
        assert_eq!(
            parse_target("https://github.com/owner/repo/pull/7#discussion")
                .unwrap()
                .2,
            Some(7)
        );
        // Upstream's parser also rejects a scheme-less host prefix.
        assert_eq!(
            parse_target("github.com/owner/repo").unwrap_err().code,
            "BAD_INPUT"
        );
        assert_eq!(parse_target("not a repo").unwrap_err().code, "BAD_INPUT");
    }

    #[test]
    fn repo_items_keep_the_upstream_raw_keys() {
        let repo = json!({
            "full_name": "a/b",
            "html_url": "https://github.com/a/b",
            "owner": {"login": "a"},
            "created_at": "2020-01-01T00:00:00Z",
            "description": "desc",
            "stargazers_count": 10,
            "license": {"spdx_id": "MIT"}
        });
        let item = repo_item(&repo, None);
        assert_eq!(item["title"], "a/b");
        assert_eq!(item["author"], "a");
        assert_eq!(item["text"], "desc");
        assert_eq!(item["raw"]["stargazers_count"], 10);
        assert_eq!(item["raw"]["license"], "MIT");
        assert_eq!(item["raw"]["topics"], Json::Null);
    }
}
