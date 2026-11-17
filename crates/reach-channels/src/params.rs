//! Parameter extraction and small text/time helpers shared by backends.

use crate::Result;
use reach_core::ReachError;
use serde_json::Value;

/// Required string parameter.
pub fn req_str(params: &Value, key: &str) -> Result<String> {
    match params.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        Some(Value::Number(number)) => Ok(number.to_string()),
        Some(Value::Null) | None => Err(ReachError::new(
            "BAD_INPUT",
            format!("missing required argument: {key}"),
        )),
        Some(_) => Err(ReachError::new(
            "BAD_INPUT",
            format!("{key} must be a string"),
        )),
    }
}

/// Optional string parameter; empty strings and nulls collapse to `None`.
pub fn opt_str(params: &Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

/// Integer parameter with a default; positive-only when `min` says so.
pub fn int_param(params: &Value, key: &str, default: i64, min: i64) -> Result<i64> {
    let value = match params.get(key) {
        None | Some(Value::Null) => default,
        Some(Value::Number(number)) => match number.as_i64() {
            Some(value) => value,
            None => {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    format!("{key} must be an integer"),
                ))
            }
        },
        Some(Value::String(text)) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| ReachError::new("BAD_INPUT", format!("{key} must be an integer")))?,
        Some(_) => {
            return Err(ReachError::new(
                "BAD_INPUT",
                format!("{key} must be an integer"),
            ))
        }
    };
    if value < min {
        return Err(ReachError::new(
            "BAD_INPUT",
            format!("{key} must be at least {min}"),
        ));
    }
    Ok(value)
}

/// Unix seconds to ISO 8601 UTC, mirroring upstream `unix_to_iso`.
pub fn unix_to_iso(value: Option<&Value>) -> Option<String> {
    let seconds = match value? {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    if seconds <= 0 {
        return None;
    }
    let moment = chrono::DateTime::from_timestamp(seconds, 0)?;
    Some(moment.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Remove HTML tags and decode character references once.
pub fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(character),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded.replace('\u{a0}', " ").trim().to_string()
}

/// Percent-encode a query value the way `urllib.parse.quote` does for text.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Render a JSON value as a plain string the way Python's f-string would.
pub fn json_scalar(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_and_optional_strings() {
        let params = json!({"query": " rust ", "limit": 3});
        assert_eq!(req_str(&params, "query").unwrap(), "rust");
        assert_eq!(req_str(&params, "limit").unwrap(), "3");
        assert_eq!(req_str(&params, "target").unwrap_err().code, "BAD_INPUT");
        assert_eq!(opt_str(&params, "missing"), None);
    }

    #[test]
    fn integer_bounds_are_enforced() {
        assert_eq!(int_param(&json!({"limit": 5}), "limit", 10, 1).unwrap(), 5);
        assert_eq!(int_param(&json!({}), "limit", 10, 1).unwrap(), 10);
        assert_eq!(
            int_param(&json!({"limit": "7"}), "limit", 10, 1).unwrap(),
            7
        );
        assert_eq!(
            int_param(&json!({"limit": 0}), "limit", 10, 1)
                .unwrap_err()
                .code,
            "BAD_INPUT"
        );
        assert_eq!(
            int_param(&json!({"limit": "x"}), "limit", 10, 1)
                .unwrap_err()
                .code,
            "BAD_INPUT"
        );
    }

    #[test]
    fn unix_seconds_to_iso_utc() {
        assert_eq!(
            unix_to_iso(Some(&json!(1_700_000_000))).unwrap(),
            "2023-11-14T22:13:20Z"
        );
        assert_eq!(unix_to_iso(Some(&json!(0))), None);
        assert_eq!(unix_to_iso(Some(&json!("bad"))), None);
    }

    #[test]
    fn html_is_stripped_and_unescaped_once() {
        assert_eq!(strip_html("<p>a  b</p>&amp;c"), "a  b&c");
    }
}
