//! `bilibili` channel: the public video API for `read` and `hot`.
//!
//! `search` and `subtitle` belong to bili-cli (which needs its own runtime), so
//! this module only implements the two unauthenticated public-API backends.
//! yt-dlp is deliberately absent upstream: Bilibili's risk control 412-blocks it.

use crate::http::{self, Endpoints};
use crate::params::{int_param, req_str, unix_to_iso};
use crate::{session, tool};
use crate::{Result, Value};
use reach_core::{make_item, ReachError};
use serde_json::{json, Value as Json};
use std::sync::LazyLock;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const REFERER: &str = "https://www.bilibili.com/";
const LOGIN_HINT: &str = "Bilibili subtitles need a login. Ask the user to run `arana login bilibili` and scan the QR code themselves; never log in on their behalf.";

/// BV id from a BV id, a video link, or a b23.tv short link.
pub async fn bvid(target: &str, timeout: Duration) -> Result<String> {
    static BV: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"BV[0-9A-Za-z]{10}").expect("bv regex"));
    let text = target.trim();
    if let Some(found) = BV.find(text) {
        return Ok(found.as_str().to_string());
    }
    if text.contains("b23.tv/") {
        let url = if text.contains("://") {
            text.to_string()
        } else {
            format!("https://{text}")
        };
        let resolved = http::resolve_redirect(&url, timeout, &[("User-Agent", UA)]).await?;
        if let Some(found) = BV.find(&resolved) {
            return Ok(found.as_str().to_string());
        }
    }
    Err(ReachError::new(
        "BAD_INPUT",
        "target must be a BV id or a bilibili.com / b23.tv video link",
    ))
}

fn video_item(video: &Json, text: Option<String>) -> Value {
    make_item(
        video
            .get("title")
            .and_then(Json::as_str)
            .map(str::to_string),
        video
            .get("bvid")
            .and_then(Json::as_str)
            .map(|bvid| format!("https://www.bilibili.com/video/{bvid}")),
        video
            .get("owner")
            .and_then(|owner| owner.get("name"))
            .and_then(Json::as_str)
            .map(str::to_string),
        unix_to_iso(video.get("pubdate")),
        text.or_else(|| video.get("desc").and_then(Json::as_str).map(str::to_string)),
        json!({
            "bvid": video.get("bvid"),
            "aid": video.get("aid"),
            "duration": video.get("duration"),
            "stat": video.get("stat"),
            "tname": video.get("tname"),
        }),
    )
}

async fn api_json(path: &str, timeout: Duration, endpoints: &Endpoints) -> Result<Json> {
    let url = format!("{}{path}", endpoints.bilibili_api.trim_end_matches('/'));
    let body = http::get_text(
        &url,
        timeout,
        &[("User-Agent", UA), ("Referer", REFERER)],
        http::DEFAULT_MAX_BYTES,
    )
    .await?;
    let data: Json = serde_json::from_str(&body)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Bilibili returned a verification page"))?;
    let code = data.get("code").and_then(Json::as_i64).unwrap_or(0);
    if code == -404 {
        return Err(ReachError::new("NOT_FOUND", "video not found"));
    }
    if code != 0 {
        let message = data
            .get("message")
            .and_then(Json::as_str)
            .unwrap_or_default();
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("Bilibili API code {code}: {message}"),
        ));
    }
    Ok(data.get("data").cloned().unwrap_or_else(|| json!({})))
}

/// `bilibili-api` backend: `read` and `hot`.
pub async fn api_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    match action {
        "read" => {
            let target = req_str(params, "target")?;
            let id = bvid(&target, timeout).await?;
            let data = api_json(
                &format!("/x/web-interface/view?bvid={id}"),
                timeout,
                endpoints,
            )
            .await?;
            Ok(vec![video_item(&data, None)])
        }
        "hot" => {
            let limit = int_param(params, "limit", 10, 0)?;
            let data = api_json(
                &format!("/x/web-interface/popular?ps={limit}&pn=1"),
                timeout,
                endpoints,
            )
            .await?;
            let videos = data
                .get("list")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(videos
                .iter()
                .take(limit.max(0) as usize)
                .map(|video| video_item(video, None))
                .collect())
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("bilibili has no public-API action {other}"),
        )),
    }
}

/// Map a bili-cli error envelope to the unified codes.
fn cli_error(payload: &Json) -> ReachError {
    let error = payload.get("error").cloned().unwrap_or(Json::Null);
    let code = error
        .get("code")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let message = error
        .get("message")
        .and_then(Json::as_str)
        .unwrap_or("bili-cli failed")
        .to_string();
    let upstream_hint = error
        .get("hint")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|hint| !hint.is_empty());
    if ["risk", "captcha", "verification"]
        .iter()
        .any(|word| code.contains(word))
    {
        return tool::with_hint(ReachError::new("PLATFORM_BLOCKED", message), upstream_hint);
    }
    if code.contains("rate") || message.contains("频繁") {
        return ReachError::new("RATE_LIMITED", message);
    }
    if code == "not_found" {
        return ReachError::new("NOT_FOUND", message);
    }
    if ["login", "auth", "credential"]
        .iter()
        .any(|word| code.contains(word))
    {
        let mut error = ReachError::new("NEED_LOGIN", message);
        error.hint = LOGIN_HINT.to_string();
        return error;
    }
    tool::with_hint(ReachError::new("UPSTREAM_BROKEN", message), upstream_hint)
}

async fn bili_json(
    args: Vec<String>,
    timeout: Duration,
    state_dir: &std::path::Path,
) -> Result<Json> {
    let program = tool::program("bili", "ARANA_BILI_BIN", state_dir)?;
    let env = session::session_env("bili-cli", state_dir)?;
    let mut argv = args;
    argv.push("--json".into());
    let output = tool::run(&program, &argv, env, timeout, "bili-cli").await?;
    if !output.success() && !output.stdout.contains('{') {
        return Err(tool::failed_exit(&output, "bili-cli"));
    }
    let payload = tool::json_payload(&output.stdout, "bili-cli")?;
    if payload.get("ok") == Some(&Json::Bool(false)) {
        return Err(cli_error(&payload));
    }
    Ok(payload.get("data").cloned().unwrap_or_else(|| json!({})))
}

fn cli_video_item(video: &Json, text: Option<String>) -> Value {
    let owner = video.get("owner").and_then(|owner| owner.get("name"));
    let url = video
        .get("url")
        .and_then(Json::as_str)
        .map(str::to_string)
        .or_else(|| {
            video
                .get("bvid")
                .and_then(Json::as_str)
                .map(|bvid| format!("https://www.bilibili.com/video/{bvid}"))
        });
    make_item(
        video
            .get("title")
            .and_then(Json::as_str)
            .map(str::to_string),
        url,
        owner
            .or_else(|| video.get("author"))
            .and_then(Json::as_str)
            .map(str::to_string),
        None,
        text.or_else(|| {
            video
                .get("description")
                .and_then(Json::as_str)
                .map(str::to_string)
        }),
        video.clone(),
    )
}

/// `bili-cli` backend: search, read, hot and subtitle through the pinned CLI
/// with `HOME` pointed at its private session home.
pub async fn cli_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &std::path::Path,
) -> Result<Vec<Value>> {
    match action {
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 0)?;
            let data = bili_json(
                vec![
                    "search".into(),
                    query,
                    "--type".into(),
                    "video".into(),
                    "-n".into(),
                    limit.to_string(),
                ],
                timeout,
                state_dir,
            )
            .await?;
            let videos = data.as_array().cloned().unwrap_or_default();
            Ok(videos
                .iter()
                .take(limit.max(0) as usize)
                .map(|video| cli_video_item(video, None))
                .collect())
        }
        "read" => {
            let target = req_str(params, "target")?;
            let id = bvid(&target, timeout).await?;
            let data = bili_json(vec!["video".into(), id], timeout, state_dir).await?;
            let video = data.get("video").cloned().unwrap_or_else(|| json!({}));
            Ok(vec![cli_video_item(&video, None)])
        }
        "hot" => {
            let limit = int_param(params, "limit", 10, 0)?;
            let data = bili_json(
                vec!["hot".into(), "-n".into(), limit.to_string()],
                timeout,
                state_dir,
            )
            .await?;
            let videos = data
                .get("items")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(videos
                .iter()
                .take(limit.max(0) as usize)
                .map(|video| cli_video_item(video, None))
                .collect())
        }
        "subtitle" => {
            let target = req_str(params, "target")?;
            let id = bvid(&target, timeout).await?;
            let data = bili_json(
                vec!["video".into(), id, "--subtitle".into()],
                timeout,
                state_dir,
            )
            .await?;
            let subtitle = data.get("subtitle").cloned().unwrap_or_else(|| json!({}));
            if subtitle.get("available") != Some(&Json::Bool(true)) {
                // Without a saved login, upstream could only say "unavailable";
                // never conflate that with "this video has no subtitles".
                if !session::has_login_state("bili-cli", state_dir)? {
                    let mut error = ReachError::new("NEED_LOGIN", "Bilibili is not logged in");
                    error.hint = LOGIN_HINT.to_string();
                    return Err(error);
                }
                let unavailable =
                    data.get("warnings")
                        .and_then(Json::as_array)
                        .is_some_and(|warnings| {
                            warnings.iter().any(|warning| {
                                warning.get("code").and_then(Json::as_str)
                                    == Some("subtitle_unavailable")
                            })
                        });
                if unavailable {
                    return Err(ReachError::new(
                        "UPSTREAM_BROKEN",
                        "Bilibili could not retrieve subtitles; this does not prove the video has none",
                    ));
                }
                return Err(ReachError::new("NOT_FOUND", "this video has no subtitles"));
            }
            let text = subtitle
                .get("text")
                .and_then(Json::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    subtitle
                        .get("items")
                        .and_then(Json::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.get("content").and_then(Json::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default()
                });
            let video = data.get("video").cloned().unwrap_or_else(|| json!({}));
            Ok(vec![cli_video_item(&video, Some(text))])
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("bili-cli has no action {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bv_ids_from_links() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            runtime
                .block_on(bvid("BV1GJ411x7h7", Duration::from_secs(1)))
                .unwrap(),
            "BV1GJ411x7h7"
        );
        assert_eq!(
            runtime
                .block_on(bvid(
                    "https://www.bilibili.com/video/BV1GJ411x7h7?p=1",
                    Duration::from_secs(1)
                ))
                .unwrap(),
            "BV1GJ411x7h7"
        );
        assert_eq!(
            runtime
                .block_on(bvid("https://example.com/x", Duration::from_secs(1)))
                .unwrap_err()
                .code,
            "BAD_INPUT"
        );
    }

    #[test]
    fn video_items_have_the_public_api_shape() {
        let video = json!({
            "bvid": "BV1GJ411x7h7",
            "aid": 1,
            "title": "T",
            "desc": "D",
            "pubdate": 1_700_000_000,
            "owner": {"name": "up"},
            "duration": 10,
            "stat": {"view": 1},
            "tname": "tech"
        });
        let item = video_item(&video, None);
        assert_eq!(item["title"], "T");
        assert_eq!(item["author"], "up");
        assert_eq!(item["url"], "https://www.bilibili.com/video/BV1GJ411x7h7");
        assert_eq!(item["published_at"], "2023-11-14T22:13:20Z");
        assert_eq!(item["raw"]["tname"], "tech");
    }
}
