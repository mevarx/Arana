//! `web` channel: Exa search, and page reading through Jina Reader with Exa as
//! the declared fallback.

use crate::http::{self, Endpoints, JINA_MAX_BYTES};
use crate::params::{int_param, req_str};
use crate::urlsec::normalize_public_http_url;
use crate::{Result, Value};
use reach_core::{make_item, ReachError};
use serde_json::{json, Value as Json};
use std::sync::LazyLock;
use std::time::Duration;

const JINA_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const ANTIBOT_SCAN_BYTES: usize = 4096;

fn public_url(target: &str) -> Result<String> {
    normalize_public_http_url(target)
        .map_err(|_| ReachError::new("BAD_INPUT", "target must be a public http(s) URL"))
}

/// Recognize high-confidence Jina/Cloudflare challenge responses.
fn is_antibot_page(body: &str) -> bool {
    let sample: String = body
        .chars()
        .take(ANTIBOT_SCAN_BYTES)
        .collect::<String>()
        .to_lowercase();
    let jina_captcha_warning = sample.contains("warning:") && sample.contains("requiring captcha");
    let challenge_structure = sample.contains("title: just a moment...")
        || sample.contains("## performing security verification")
        || sample.contains("title: attention required! | cloudflare");
    let cloudflare_block = sample.contains("title: attention required! | cloudflare")
        && (sample.contains("ray id") || sample.contains("/cdn-cgi/challenge-platform/"));
    (jina_captcha_warning && challenge_structure) || cloudflare_block
}

fn parse_jina(url: &str, body: &str) -> Value {
    static TITLE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?m)^Title:\s*(.*)$").expect("title regex"));
    static PUBLISHED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?m)^Published Time:\s*(.*)$").expect("date regex"));
    let title = TITLE
        .captures(body)
        .map(|capture| capture[1].trim().to_string());
    let published = PUBLISHED
        .captures(body)
        .map(|capture| capture[1].trim().to_string());
    let content = match body.split_once("Markdown Content:") {
        Some((_, rest)) => rest,
        None => body,
    };
    make_item(
        title,
        Some(url.to_string()),
        None,
        published,
        Some(content.trim().to_string()),
        Json::Null,
    )
}

/// Split Exa's `Title: / URL: / Published: / Author:` blocks into items.
///
/// Blocks start at a line beginning with `Title: ` (upstream splits on a
/// newline before that marker), so a `Title:` inside body text never splits.
fn parse_exa_text(text: &str) -> Vec<Value> {
    static HEADER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(Title|URL|Published|Author):\s*(.*)$").expect("exa header regex")
    });
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.trim().lines() {
        if line.starts_with("Title: ") && !current.is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let mut items = Vec::new();
    for block in blocks {
        if !block
            .first()
            .map(|line| line.starts_with("Title:"))
            .unwrap_or(false)
        {
            continue;
        }
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut body_start = block.len();
        for (index, line) in block.iter().enumerate() {
            match HEADER.captures(line) {
                Some(capture) => {
                    fields.push((capture[1].to_lowercase(), capture[2].trim().to_string()))
                }
                None => {
                    body_start = index;
                    break;
                }
            }
        }
        let body: Vec<&str> = block
            .iter()
            .skip(body_start)
            .copied()
            .filter(|line| !matches!(line.trim(), "Highlights:" | "Text:"))
            .collect();
        let field = |name: &str| -> Option<String> {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty() && value != "N/A")
        };
        let raw: Json = fields
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect::<serde_json::Map<String, Json>>()
            .into();
        items.push(make_item(
            field("title"),
            field("url"),
            field("author"),
            field("published"),
            Some(body.join("\n").trim().to_string()),
            raw,
        ));
    }
    items
}

/// One `tools/call` against Exa's free hosted MCP (no key).
async fn exa_call(
    tool: &str,
    arguments: Json,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    });
    // `post_json_text` sets `Content-Type: application/json` itself; adding a
    // second value here makes Exa reject the request with HTTP 415.
    let raw = http::post_json_text(
        &endpoints.exa_mcp,
        &body,
        timeout,
        &[("Accept", "application/json, text/event-stream")],
        http::DEFAULT_MAX_BYTES,
    )
    .await?;
    // The hosted MCP answers as an SSE stream; the last `data:` frame carries the reply.
    let payload = raw
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .next_back()
        .unwrap_or_else(|| raw.trim());
    let message: Json = serde_json::from_str(payload)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Exa returned an unreadable response"))?;
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        let code = error.get("code").and_then(Json::as_i64);
        return Err(exa_error(&text, code));
    }
    let result = message.get("result").cloned().unwrap_or_else(|| json!({}));
    let text = result
        .get("content")
        .and_then(Json::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Json::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Json::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if result.get("isError").and_then(Json::as_bool) == Some(true) {
        return Err(exa_error(&text, None));
    }
    Ok(text)
}

fn exa_error(text: &str, rpc_code: Option<i64>) -> ReachError {
    static LIMIT: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b429\b|rate[ _-]?limit|too many requests|\bquota\b|(?:usage|request|daily|monthly|free)[ _-]+limit|(?:credit|credits|balance).{0,40}(?:exhaust|deplet|insufficient|exceed|reach)|(?:out of|insufficient|exhausted).{0,20}credits?",
        )
        .expect("exa limit regex")
    });
    let excerpt: String = text.chars().take(300).collect();
    if rpc_code == Some(-32602) {
        return ReachError::new(
            "BAD_INPUT",
            format!("Exa rejected the arguments: {excerpt}"),
        );
    }
    if rpc_code == Some(429) || LIMIT.is_match(text) {
        let mut error = ReachError::new("RATE_LIMITED", format!("Exa: {excerpt}"));
        error.hint =
            "Exa's hosted search limit was reached. Use the agent's built-in web search or wait before retrying."
                .into();
        return error;
    }
    ReachError::new("UPSTREAM_BROKEN", format!("Exa: {excerpt}"))
}

/// `web search` — Exa semantic search.
pub async fn exa_search(
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    let query = req_str(params, "query")?;
    let limit = int_param(params, "limit", 5, 1)?;
    let text = exa_call(
        "web_search_exa",
        json!({"query": query, "objective": query, "numResults": limit}),
        timeout,
        endpoints,
    )
    .await?;
    let mut items = parse_exa_text(&text);
    if items.is_empty() {
        // An unrecognized payload is not a verified zero-hit search.
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "Exa returned no recognizable search results",
        ));
    }
    items.truncate(limit as usize);
    Ok(items)
}

/// `web read` via Exa — used when Jina fails.
pub async fn exa_read(
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    let target = public_url(&req_str(params, "target")?)?;
    let text = exa_call(
        "web_fetch_exa",
        json!({"urls": [target], "maxCharacters": 20000}),
        timeout,
        endpoints,
    )
    .await?;
    let mut items = parse_exa_text(&text);
    if !items.is_empty() {
        if let Some(item) = items.first_mut() {
            let empty = item.get("url").map(Value::is_null).unwrap_or(true);
            if empty {
                item["url"] = json!(target);
            }
        }
        items.truncate(1);
        return Ok(items);
    }
    if text.trim().is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "Exa returned no content",
        ));
    }
    Ok(vec![make_item(
        None,
        Some(target),
        None,
        None,
        Some(text.trim().to_string()),
        Json::Null,
    )])
}

/// `web read` via Jina Reader.
pub async fn jina_read(
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    let target = public_url(&req_str(params, "target")?)?;
    let url = format!("{}/{}", endpoints.jina.trim_end_matches('/'), target);
    let body = http::get_text(
        &url,
        timeout,
        &[("User-Agent", JINA_UA), ("Accept", "text/plain")],
        JINA_MAX_BYTES,
    )
    .await?;
    if is_antibot_page(&body) {
        let mut error = ReachError::new(
            "UPSTREAM_BROKEN",
            "Jina Reader returned an anti-bot challenge page; the target content was not fetched. Use a site tool or device browser instead.",
        );
        error.hint = String::new();
        return Err(error);
    }
    Ok(vec![parse_jina(&target, &body)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jina_headers_and_content() {
        let body = "Title: Example Domain\n\nURL Source: https://example.com\n\nPublished Time: 2024-01-02\n\nMarkdown Content:\n# Hi\n\nBody text\n";
        let item = parse_jina("https://example.com", body);
        assert_eq!(item["title"], "Example Domain");
        assert_eq!(item["published_at"], "2024-01-02");
        assert_eq!(item["text"], "# Hi\n\nBody text");
        assert_eq!(item["url"], "https://example.com");
    }

    #[test]
    fn parses_exa_result_blocks() {
        let text = "Title: First\nURL: https://a.test/1\nPublished: N/A\nAuthor: Ada\nHighlights:\nBody one\n\
                    Title: Second\nURL: https://b.test/2\n\nBody two";
        let items = parse_exa_text(text);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "First");
        assert_eq!(items[0]["author"], "Ada");
        assert_eq!(items[0]["published_at"], Json::Null);
        assert_eq!(items[0]["text"], "Body one");
        assert_eq!(items[1]["url"], "https://b.test/2");
        assert_eq!(items[1]["text"], "Body two");
    }

    #[test]
    fn detects_challenge_pages() {
        // Upstream needs both the captcha warning and the challenge structure.
        assert!(is_antibot_page(
            "Warning: This request is requiring captcha verification\nTitle: Just a moment...\n"
        ));
        assert!(!is_antibot_page("Title: Just a moment...\n"));
        assert!(!is_antibot_page(
            "Title: Example Domain\nMarkdown Content:\nhello"
        ));
        assert!(is_antibot_page(
            "Title: Attention Required! | Cloudflare\nRay ID: 123\n/cdn-cgi/challenge-platform/h/b\n"
        ));
    }

    #[test]
    fn rejects_private_targets() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = runtime
            .block_on(jina_read(
                &json!({"target": "http://127.0.0.1/"}),
                Duration::from_secs(1),
                &Endpoints::default(),
            ))
            .unwrap_err();
        assert_eq!(error.code, "BAD_INPUT");
    }

    #[test]
    fn classifies_exa_errors() {
        assert_eq!(exa_error("bad args", Some(-32602)).code, "BAD_INPUT");
        assert_eq!(
            exa_error("429 too many requests", None).code,
            "RATE_LIMITED"
        );
        assert_eq!(exa_error("out of credits", None).code, "RATE_LIMITED");
        assert_eq!(exa_error("boom", None).code, "UPSTREAM_BROKEN");
    }
}
