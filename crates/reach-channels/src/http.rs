//! Bounded HTTP for native backends.
//!
//! One shared client (rustls), per-request timeouts driven by the caller's
//! remaining budget, and a hard body cap so a hostile or broken upstream cannot
//! exhaust memory. Errors map onto the core error codes via `From<reqwest::Error>`.

use crate::Result;
use reach_core::ReachError;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

/// Upstream's generic body cap (`http_request` default).
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Jina Reader cap.
pub const JINA_MAX_BYTES: usize = 5 * 1024 * 1024;
/// V2EX API cap.
pub const V2EX_MAX_BYTES: usize = 1024 * 1024;

pub const USER_AGENT: &str = concat!("agent-reach/", "1.6.1");

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
});

/// Base URLs for every native upstream, overridable in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub v2ex: String,
    pub github_api: String,
    pub tencent_quote: String,
    pub tencent_search: String,
    pub xueqiu: String,
    pub xueqiu_stock: String,
    /// Homepage used to bootstrap Xueqiu's public session cookie.
    pub xueqiu_home: String,
    pub bilibili_api: String,
    pub jina: String,
    pub exa_mcp: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            v2ex: "https://www.v2ex.com".into(),
            github_api: "https://api.github.com".into(),
            tencent_quote: "https://qt.gtimg.cn".into(),
            tencent_search: "https://smartbox.gtimg.cn".into(),
            xueqiu: "https://xueqiu.com".into(),
            xueqiu_stock: "https://stock.xueqiu.com".into(),
            xueqiu_home: "https://xueqiu.com".into(),
            bilibili_api: "https://api.bilibili.com".into(),
            jina: "https://r.jina.ai".into(),
            exa_mcp: "https://mcp.exa.ai/mcp".into(),
        }
    }
}

async fn read_bounded(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let overflow = || ReachError::new("UPSTREAM_BROKEN", format!("response exceeds {max} bytes"));
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(overflow());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(overflow());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn body_capped(bytes: &[u8], max: usize) -> Result<String> {
    if bytes.len() > max {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("response exceeds {max} bytes"),
        ));
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// GET a bounded body as text.
pub async fn get_text(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<String> {
    let mut request = CLIENT.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status();
    let bytes = read_bounded(response, max_bytes).await?;
    if !status.is_success() {
        return Err(status_error(status.as_u16(), &bytes));
    }
    body_capped(&bytes, max_bytes)
}

/// GET a bounded body as raw bytes (for non-UTF-8 upstreams such as Tencent).
pub async fn get_bytes(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut request = CLIENT.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status();
    let bytes = read_bounded(response, max_bytes).await?;
    if !status.is_success() {
        return Err(status_error(status.as_u16(), &bytes));
    }
    if bytes.len() > max_bytes {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("response exceeds {max_bytes} bytes"),
        ));
    }
    Ok(bytes.to_vec())
}

/// GET and parse JSON, mapping a decode failure to `UPSTREAM_BROKEN`.
pub async fn get_json(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<Value> {
    let text = get_text(url, timeout, headers, max_bytes).await?;
    serde_json::from_str(&text).map_err(|_| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "upstream returned an unreadable JSON response",
        )
    })
}

/// POST a JSON body and return the bounded text response.
pub async fn post_json_text(
    url: &str,
    body: &Value,
    timeout: Duration,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<String> {
    let mut request = CLIENT.post(url).timeout(timeout).json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status();
    let bytes = read_bounded(response, max_bytes).await?;
    if !status.is_success() {
        return Err(status_error(status.as_u16(), &bytes));
    }
    body_capped(&bytes, max_bytes)
}

fn status_error(status: u16, _body: &[u8]) -> ReachError {
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("Unknown status");
    match status {
        404 => ReachError::new("NOT_FOUND", format!("HTTP 404: {reason}")),
        429 => ReachError::new("RATE_LIMITED", "HTTP 429"),
        _ => ReachError::new("UPSTREAM_BROKEN", format!("HTTP {status}: {reason}")),
    }
}

/// A response whose status is inspected by the caller (headers matter for
/// rate-limit classification, so this never turns a status into an error).
pub struct RawResponse {
    pub status: u16,
    pub rate_limit_remaining: Option<String>,
    pub retry_after: Option<String>,
    pub body: String,
}

/// GET without status-based error mapping; the body is still bounded.
pub async fn get_raw(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<RawResponse> {
    let mut request = CLIENT.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let rate_limit_remaining = header("x-ratelimit-remaining");
    let retry_after = header("retry-after");
    let bytes = read_bounded(response, max_bytes).await?;
    let body = body_capped(&bytes, max_bytes)?;
    Ok(RawResponse {
        status,
        rate_limit_remaining,
        retry_after,
        body,
    })
}

/// GET the page and return the `Set-Cookie` name/value pairs it carries.
///
/// Xueqiu hands an anti-DDoS cookie (`acw_tc`) to a first-time visitor; the
/// public endpoints only answer once that cookie is replayed. We keep the pairs
/// explicitly instead of enabling a persistent cookie store.
pub async fn get_capture_cookies(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
) -> Result<Vec<(String, String)>> {
    let mut request = CLIENT.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(status_error(response.status().as_u16(), &[]));
    }
    let mut pairs = Vec::new();
    for header in response.headers().get_all("set-cookie") {
        let Ok(text) = header.to_str() else { continue };
        let pair = text.split(';').next().unwrap_or_default().trim();
        if let Some((name, value)) = pair.split_once('=') {
            if !name.is_empty() && !value.is_empty() {
                pairs.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    Ok(pairs)
}

/// Follow redirects without downloading the page; returns the final URL.
pub async fn resolve_redirect(
    url: &str,
    timeout: Duration,
    headers: &[(&str, &str)],
) -> Result<String> {
    let mut request = CLIENT.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    Ok(response.url().to_string())
}
