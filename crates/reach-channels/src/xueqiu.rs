//! `xueqiu` channel: Tencent quote/search first, then Xueqiu's own API.
//!
//! `quote` and `search` need no login: Tencent's endpoints answer anonymously.
//! The Xueqiu API (`xueqiu-api`) needs a session cookie for the community
//! actions, and reports `NEED_LOGIN` instead of guessing when it has none.

use crate::http::{self, Endpoints};
use crate::params::{encode_component, int_param, json_scalar, req_str, strip_html};
use crate::{Result, Value};
use reach_core::{make_item, ReachError};
use serde_json::{json, Map, Value as Json};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

const UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const REFERER: &str = "https://xueqiu.com/";

fn login_hint() -> String {
    "Xueqiu community data needs a browser session. Run `arana login xueqiu`, let the user open \
     xueqiu.com in the dedicated browser, then run `arana login xueqiu --sync`. The session \
     expires within hours; repeat these steps when it does."
        .to_string()
}

fn need_login(message: &str) -> ReachError {
    let mut error = ReachError::new("NEED_LOGIN", message);
    error.hint = login_hint();
    error
}

/// SH600519 / 600519 / HK00700 / 00700 / AAPL -> Tencent quote code.
pub fn tencent_code(symbol: &str) -> Result<String> {
    static CN: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^(SH|SZ|BJ)\d{6}$").expect("cn code regex"));
    static DIGITS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\d{6}$").expect("digits regex"));
    static HK: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^HK\d{1,5}$").expect("hk code regex"));
    static SHORT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\d{1,5}$").expect("short code regex"));
    static US: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[A-Z][A-Z.\-]{0,9}$").expect("us code regex"));

    let text = symbol.trim().to_uppercase();
    if CN.is_match(&text) {
        return Ok(text.to_lowercase());
    }
    if DIGITS.is_match(&text) {
        if text.starts_with("920") {
            return Ok(format!("bj{text}"));
        }
        let first = text.chars().next().unwrap_or('0');
        let prefix = if "69".contains(first) {
            "sh"
        } else if "48".contains(first) {
            "bj"
        } else {
            "sz"
        };
        return Ok(format!("{prefix}{text}"));
    }
    if HK.is_match(&text) {
        return Ok(format!("hk{:0>5}", &text[2..]));
    }
    if SHORT.is_match(&text) {
        return Ok(format!("hk{:0>5}", text));
    }
    if US.is_match(&text) {
        return Ok(format!("us{text}"));
    }
    Err(ReachError::new(
        "BAD_INPUT",
        "symbol must look like SH600519, 600519, 00700 or AAPL",
    ))
}

/// Tencent market + code -> the symbol Xueqiu uses in URLs and its API.
pub fn xueqiu_symbol(market: &str, code: &str) -> String {
    match market.to_lowercase().as_str() {
        "sh" | "sz" | "bj" => format!("{}{code}", market.to_uppercase()),
        "hk" => code.to_string(),
        _ => code.split('.').next().unwrap_or_default().to_uppercase(),
    }
}

/// The `="..."` payload of a Tencent quote/search response, unescaped.
fn quoted_payload(body: &str) -> Option<String> {
    static QUOTED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"="([^"]*)""#).expect("quoted regex"));
    let captured = QUOTED.captures(body)?;
    let raw = captured[1].to_string();
    Some(unescape_json_string(&raw))
}

/// Undo the JSON string escaping Tencent applies inside its quoted payload.
fn unescape_json_string(raw: &str) -> String {
    let quoted = format!("\"{}\"", raw.replace('"', "\\\""));
    serde_json::from_str::<String>(&quoted).unwrap_or_else(|_| raw.to_string())
}

fn text_field(value: Option<&Json>) -> Option<String> {
    let rendered = json_scalar(value);
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// `tencent` backend: quote and search.
pub async fn tencent_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    match action {
        "quote" => {
            let symbol = req_str(params, "symbol")?;
            let code = tencent_code(&symbol)?;
            let url = format!("{}/q={code}", endpoints.tencent_quote.trim_end_matches('/'));
            let bytes = http::get_bytes(
                &url,
                timeout,
                &[("User-Agent", UA)],
                http::DEFAULT_MAX_BYTES,
            )
            .await?;
            let (decoded, _, _) = encoding_rs::GBK.decode(&bytes);
            let payload = quoted_payload(&decoded)
                .ok_or_else(|| ReachError::new("NOT_FOUND", format!("no quote for {symbol}")))?;
            if payload.matches('~').count() < 34 {
                return Err(ReachError::new(
                    "NOT_FOUND",
                    format!("no quote for {symbol}"),
                ));
            }
            let fields: Vec<&str> = payload.split('~').collect();
            let market = code.get(..2).unwrap_or("sh");
            let xq = xueqiu_symbol(market, fields[2]);
            let quote = json!({
                "symbol": xq,
                "name": fields[1],
                "current": fields[3],
                "last_close": fields[4],
                "open": fields[5],
                "chg": fields[31],
                "percent": fields[32],
                "high": fields[33],
                "low": fields[34],
                "time": fields[30],
            });
            Ok(vec![make_item(
                Some(format!("{} ({xq})", fields[1])),
                Some(format!("https://xueqiu.com/S/{xq}")),
                None,
                None,
                Some(format!("{} {} ({}%)", fields[1], fields[3], fields[32])),
                quote,
            )])
        }
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 1)?;
            let url = format!(
                "{}/s3/?v=2&q={}&t=all",
                endpoints.tencent_search.trim_end_matches('/'),
                encode_component(&query)
            );
            let body = http::get_text(
                &url,
                timeout,
                &[("User-Agent", UA)],
                http::DEFAULT_MAX_BYTES,
            )
            .await?;
            let payload = match quoted_payload(&body) {
                Some(payload) if payload != "N" && !payload.is_empty() => payload,
                _ => return Ok(Vec::new()),
            };
            let mut items = Vec::new();
            for entry in payload.split('^').take(limit as usize) {
                let parts: Vec<&str> = entry.split('~').collect();
                if parts.len() < 3 {
                    continue;
                }
                let (market, code, name) = (parts[0], parts[1], parts[2]);
                let xq = xueqiu_symbol(market, code);
                let mut raw = Map::new();
                raw.insert("symbol".into(), json!(xq));
                raw.insert("name".into(), json!(name));
                raw.insert("market".into(), json!(market));
                raw.insert(
                    "type".into(),
                    match parts.get(4) {
                        Some(value) => json!(value),
                        None => Json::Null,
                    },
                );
                items.push(make_item(
                    Some(format!("{name} ({xq})")),
                    Some(format!("https://xueqiu.com/S/{xq}")),
                    None,
                    None,
                    None,
                    Json::Object(raw),
                ));
            }
            Ok(items)
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("xueqiu has no action {other}"),
        )),
    }
}

/// Session cookies: a saved `xueqiu_cookie`, else the homepage's anti-DDoS pair.
async fn session_cookies(state_dir: &Path, homepage: &str) -> Vec<(String, String)> {
    if let Some(cookie) = config_cookie(state_dir) {
        return cookie
            .split(';')
            .filter_map(|pair| pair.trim().split_once('='))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
    }
    http::get_capture_cookies(homepage, Duration::from_secs(10), &[("User-Agent", UA)])
        .await
        .unwrap_or_default()
}

fn config_cookie(state_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_dir.join("config.yaml")).ok()?;
    let parsed: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    parsed
        .get("xueqiu_cookie")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn cookie_header(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn xq_json(url: &str, timeout: Duration, cookies: &[(String, String)]) -> Result<Json> {
    let cookie = cookie_header(cookies);
    let headers: Vec<(&str, &str)> = vec![
        ("User-Agent", UA),
        ("Referer", REFERER),
        ("Cookie", cookie.as_str()),
    ];
    let response = http::get_raw(url, timeout, &headers, http::DEFAULT_MAX_BYTES).await?;
    match response.status {
        200..=299 => {}
        401 => return Err(need_login("Xueqiu rejected the session")),
        400 => {
            let error_code = serde_json::from_str::<Json>(&response.body)
                .ok()
                .and_then(|body| body.get("error_code").map(|value| json_scalar(Some(value))))
                .unwrap_or_default();
            if error_code == "400016" {
                return Err(need_login("Xueqiu requires a logged-in session"));
            }
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!("Xueqiu API returned HTTP 400 (error_code {error_code})"),
            ));
        }
        status => {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                format!("Xueqiu API returned HTTP {status}"),
            ))
        }
    }
    serde_json::from_str(&response.body).map_err(|_| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "Xueqiu returned an unreadable response; login expiry was not established",
        )
    })
}

/// Each timeline row carries a JSON-encoded `data` field with the real post.
fn parse_hot_posts(data: &Json, limit: i64) -> Vec<Json> {
    let mut results = Vec::new();
    let rows = data
        .get("list")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    for row in rows.iter().take(limit.max(0) as usize) {
        let post: Json = row
            .get("data")
            .and_then(Json::as_str)
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_else(|| json!({}));
        let user = post.get("user").cloned().unwrap_or_else(|| json!({}));
        let body = post
            .get("text")
            .and_then(Json::as_str)
            .or_else(|| post.get("description").and_then(Json::as_str))
            .unwrap_or_default();
        let stripped = strip_html(body);
        let target = post
            .get("target")
            .and_then(Json::as_str)
            .unwrap_or_default();
        results.push(json!({
            "id": post.get("id").cloned().unwrap_or(json!(0)),
            "title": post.get("title").and_then(Json::as_str).unwrap_or_default(),
            "text": stripped.chars().take(200).collect::<String>(),
            "author": user.get("screen_name").and_then(Json::as_str).unwrap_or_default(),
            "likes": post.get("like_count").cloned().unwrap_or(json!(0)),
            "url": if target.is_empty() { String::new() } else { format!("https://xueqiu.com{target}") },
        }));
    }
    results
}

/// `xueqiu-api` backend.
pub async fn api_backend(
    action: &str,
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    match action {
        "quote" => {
            let symbol = req_str(params, "symbol")?;
            let code = tencent_code(&symbol)?;
            let xq = xueqiu_symbol(code.get(..2).unwrap_or("sh"), code.get(2..).unwrap_or(""));
            let cookies = session_cookies(state_dir, &endpoints.xueqiu_home).await;
            let url = format!(
                "{}/v5/stock/quote.json?symbol={}&extend=detail",
                endpoints.xueqiu_stock.trim_end_matches('/'),
                encode_component(&xq)
            );
            let data = xq_json(&url, timeout, &cookies).await?;
            let quote = data
                .get("data")
                .and_then(|data| data.get("quote"))
                .cloned()
                .filter(|quote| !quote.is_null());
            let quote = quote
                .ok_or_else(|| ReachError::new("NOT_FOUND", format!("no quote for {symbol}")))?;
            let name = json_scalar(quote.get("name"));
            let symbol_field = json_scalar(quote.get("symbol"));
            Ok(vec![make_item(
                Some(format!("{name} ({symbol_field})")),
                Some(format!("https://xueqiu.com/S/{symbol_field}")),
                None,
                None,
                Some(format!(
                    "{name} {} ({}%)",
                    json_scalar(quote.get("current")),
                    json_scalar(quote.get("percent"))
                )),
                quote,
            )])
        }
        "search" => {
            let query = req_str(params, "query")?;
            let limit = int_param(params, "limit", 5, 1)?;
            let cookies = session_cookies(state_dir, &endpoints.xueqiu_home).await;
            let url = format!(
                "{}/stock/search.json?code={}&size={limit}",
                endpoints.xueqiu.trim_end_matches('/'),
                encode_component(&query)
            );
            let data = xq_json(&url, timeout, &cookies).await?;
            let stocks = data
                .get("stocks")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(stocks
                .iter()
                .take(limit as usize)
                .map(|stock| {
                    let name = json_scalar(stock.get("name"));
                    let code = json_scalar(stock.get("code"));
                    make_item(
                        Some(format!("{name} ({code})")),
                        Some(format!("https://xueqiu.com/S/{code}")),
                        None,
                        None,
                        None,
                        stock.clone(),
                    )
                })
                .collect())
        }
        "hot_posts" => {
            let limit = int_param(params, "limit", 10, 0)?.clamp(0, 50);
            let cookies = session_cookies(state_dir, &endpoints.xueqiu_home).await;
            if !cookies.iter().any(|(name, _)| name == "xq_a_token") {
                return Err(need_login(
                    "Xueqiu community data needs a logged-in session",
                ));
            }
            let url = format!(
                "{}/v4/statuses/public_timeline_by_category.json?since_id=-1&max_id=-1&count={limit}&category=-1",
                endpoints.xueqiu.trim_end_matches('/')
            );
            let data = xq_json(&url, timeout, &cookies).await?;
            let posts = parse_hot_posts(&data, limit);
            Ok(posts
                .iter()
                .map(|post| {
                    let text = json_scalar(post.get("text"));
                    let title = match post.get("title").and_then(Json::as_str) {
                        Some(title) if !title.is_empty() => title.to_string(),
                        _ => text.chars().take(60).collect(),
                    };
                    let url = post.get("url").and_then(Json::as_str).map(str::to_string);
                    make_item(
                        Some(title),
                        url.filter(|value| !value.is_empty()),
                        text_field(post.get("author")),
                        None,
                        Some(text),
                        post.clone(),
                    )
                })
                .collect())
        }
        "hot_stocks" => {
            let limit = int_param(params, "limit", 10, 0)?;
            let cookies = session_cookies(state_dir, &endpoints.xueqiu_home).await;
            if !cookies.iter().any(|(name, _)| name == "xq_a_token") {
                return Err(need_login(
                    "Xueqiu community data needs a logged-in session",
                ));
            }
            let url = format!(
                "{}/v5/stock/hot_stock/list.json?size={limit}&type=10",
                endpoints.xueqiu_stock.trim_end_matches('/')
            );
            let data = xq_json(&url, timeout, &cookies).await?;
            let items = data
                .get("data")
                .and_then(|data| data.get("items"))
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(items
                .iter()
                .take(limit.max(0) as usize)
                .map(|stock| {
                    let name = json_scalar(stock.get("name"));
                    let code = {
                        let value = json_scalar(stock.get("code"));
                        if value.is_empty() {
                            json_scalar(stock.get("symbol"))
                        } else {
                            value
                        }
                    };
                    make_item(
                        Some(format!("{name} ({code})")),
                        Some(format!("https://xueqiu.com/S/{code}")),
                        None,
                        None,
                        Some(format!(
                            "{name} {} ({}%)",
                            json_scalar(stock.get("current")),
                            json_scalar(stock.get("percent"))
                        )),
                        stock.clone(),
                    )
                })
                .collect())
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("xueqiu has no action {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tencent_codes_cover_every_market() {
        assert_eq!(tencent_code("SH600519").unwrap(), "sh600519");
        assert_eq!(tencent_code("600519").unwrap(), "sh600519");
        assert_eq!(tencent_code("000001").unwrap(), "sz000001");
        assert_eq!(tencent_code("830799").unwrap(), "bj830799");
        assert_eq!(tencent_code("920001").unwrap(), "bj920001");
        assert_eq!(tencent_code("00700").unwrap(), "hk00700");
        assert_eq!(tencent_code("HK00700").unwrap(), "hk00700");
        assert_eq!(tencent_code("AAPL").unwrap(), "usAAPL");
        assert_eq!(tencent_code("??").unwrap_err().code, "BAD_INPUT");
    }

    #[test]
    fn xueqiu_symbols_follow_the_market() {
        assert_eq!(xueqiu_symbol("sh", "600519"), "SH600519");
        assert_eq!(xueqiu_symbol("hk", "00700"), "00700");
        assert_eq!(xueqiu_symbol("us", "AAPL"), "AAPL");
    }

    #[test]
    fn quote_payload_is_extracted_and_unescaped() {
        let body = r#"v_sh600519="1~贵州茅台~600519~1700.00~1690.00~1695.00~1~2~3~4~5~6~7~8~9~10~11~12~13~14~15~16~17~18~19~20~21~22~23~24~25~26~27~28~29~30~1700.00~0.59~1710~1680~1688";"#;
        let payload = quoted_payload(body).unwrap();
        assert!(payload.starts_with("1~贵州茅台~600519"));
        assert!(payload.matches('~').count() >= 34);
    }

    #[test]
    fn hot_posts_unwrap_the_nested_data_field() {
        let data = json!({"list": [
            {"data": json!({"id": 7, "title": "", "text": "<b>hi</b> there", "target": "/S/SH600519",
                            "user": {"screen_name": "bob"}, "like_count": 3}).to_string()}
        ]});
        let posts = parse_hot_posts(&data, 5);
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["author"], "bob");
        assert_eq!(posts[0]["text"], "hi there");
        assert_eq!(posts[0]["url"], "https://xueqiu.com/S/SH600519");
    }
}
