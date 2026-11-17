//! `v2ex` channel: the public V2EX JSON API (topics, nodes, members, replies).

use crate::http::{self, Endpoints, V2EX_MAX_BYTES};
use crate::params::{int_param, req_str, unix_to_iso};
use crate::{Result, Value};
use reach_core::{make_item, ReachError};
use serde_json::{json, Value as Json};
use std::sync::LazyLock;
use std::time::Duration;

const UA: &str = "agent-reach/1.0";

/// Query-string encoding matching `urllib.parse.urlencode` (space -> `+`).
fn form_value(value: &str) -> String {
    crate::params::encode_component(value).replace("%20", "+")
}

fn api_url(base: &str, path: &str, params: &[(&str, String)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(key, value)| format!("{key}={}", form_value(value)))
        .collect();
    if query.is_empty() {
        format!("{base}{path}")
    } else {
        format!("{base}{path}?{}", query.join("&"))
    }
}

/// Accept a topic id or a `https://www.v2ex.com/t/<id>` link.
pub fn parse_topic_id(target: &str) -> Result<i64> {
    static TOPIC_URL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(?:https?://)?(?:www\.)?v2ex\.com/t/(\d+)").expect("topic url regex")
    });
    let text = target.trim();
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        return text
            .parse::<i64>()
            .map_err(|_| ReachError::new("BAD_INPUT", "target must be a V2EX topic id"));
    }
    if let Some(capture) = TOPIC_URL.captures(text) {
        if let Ok(id) = capture[1].parse::<i64>() {
            return Ok(id);
        }
    }
    Err(ReachError::new(
        "BAD_INPUT",
        "target must be a V2EX topic link (https://www.v2ex.com/t/<id>) or id",
    ))
}

fn topic_item(topic: &Json, text: Option<String>, raw: Option<Json>) -> Value {
    let member = topic.get("member").cloned().unwrap_or_else(|| json!({}));
    make_item(
        topic
            .get("title")
            .and_then(Json::as_str)
            .map(str::to_string),
        topic.get("url").and_then(Json::as_str).map(str::to_string),
        member
            .get("username")
            .and_then(Json::as_str)
            .map(str::to_string),
        unix_to_iso(topic.get("created")),
        text.or_else(|| {
            topic
                .get("content")
                .and_then(Json::as_str)
                .map(str::to_string)
        }),
        raw.unwrap_or_else(|| topic.clone()),
    )
}

async fn get_json(url: &str, timeout: Duration) -> Result<Json> {
    http::get_json(url, timeout, &[("User-Agent", UA)], V2EX_MAX_BYTES).await
}

/// Dispatch one `v2ex` action.
pub async fn read(
    action: &str,
    params: &Value,
    timeout: Duration,
    endpoints: &Endpoints,
) -> Result<Vec<Value>> {
    let base = endpoints.v2ex.trim_end_matches('/').to_string();
    match action {
        "hot" => {
            let limit = int_param(params, "limit", 10, 0)?;
            let data = get_json(&api_url(&base, "/api/topics/hot.json", &[]), timeout).await?;
            let topics = data.as_array().cloned().unwrap_or_default();
            Ok(topics
                .iter()
                .take(limit as usize)
                .map(|topic| topic_item(topic, None, None))
                .collect())
        }
        "node" => {
            let node_name = req_str(params, "node_name")?;
            let limit = int_param(params, "limit", 10, 0)?;
            let url = api_url(
                &base,
                "/api/topics/show.json",
                &[("node_name", node_name.clone()), ("page", "1".to_string())],
            );
            let data = get_json(&url, timeout).await?;
            let topics = match data.as_array() {
                Some(topics) => topics.clone(),
                None => {
                    return Err(ReachError::new(
                        "NOT_FOUND",
                        format!("V2EX node not found: {node_name}"),
                    ))
                }
            };
            Ok(topics
                .iter()
                .take(limit as usize)
                .map(|topic| topic_item(topic, None, None))
                .collect())
        }
        "read" => {
            let topic_id = parse_topic_id(&req_str(params, "target")?)?;
            let url = api_url(
                &base,
                "/api/topics/show.json",
                &[("id", topic_id.to_string())],
            );
            let data = get_json(&url, timeout).await?;
            let topic = match data.as_array().and_then(|items| items.first()).cloned() {
                Some(topic) if topic.get("id").is_some() => topic,
                _ => {
                    return Err(ReachError::new(
                        "NOT_FOUND",
                        format!("V2EX topic not found: {topic_id}"),
                    ))
                }
            };
            let replies_url = api_url(
                &base,
                "/api/replies/show.json",
                &[
                    ("topic_id", topic_id.to_string()),
                    ("page", "1".to_string()),
                ],
            );
            let replies = get_json(&replies_url, timeout)
                .await
                .ok()
                .and_then(|data| data.as_array().cloned())
                .unwrap_or_default();
            let mut lines = vec![topic
                .get("content")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()];
            for reply in &replies {
                let who = reply
                    .get("member")
                    .and_then(|member| member.get("username"))
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let content = reply
                    .get("content")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                lines.push(format!("\n@{who}: {content}"));
            }
            Ok(vec![topic_item(
                &topic,
                Some(lines.join("\n").trim().to_string()),
                Some(json!({"topic": topic, "replies": replies})),
            )])
        }
        "user" => {
            let username = req_str(params, "username")?;
            let url = api_url(
                &base,
                "/api/members/show.json",
                &[("username", username.clone())],
            );
            let data = get_json(&url, timeout).await?;
            if data.get("id").is_none() {
                return Err(ReachError::new(
                    "NOT_FOUND",
                    format!("V2EX user not found: {username}"),
                ));
            }
            let field = |key: &str| data.get(key).and_then(Json::as_str).map(str::to_string);
            Ok(vec![make_item(
                field("username"),
                field("url"),
                field("username"),
                unix_to_iso(data.get("created")),
                field("bio"),
                data.clone(),
            )])
        }
        other => Err(ReachError::new(
            "BAD_INPUT",
            format!("v2ex has no action {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_ids_accept_ids_and_links() {
        assert_eq!(parse_topic_id("1000").unwrap(), 1000);
        assert_eq!(parse_topic_id("https://www.v2ex.com/t/1000").unwrap(), 1000);
        assert_eq!(parse_topic_id("www.v2ex.com/t/42#reply1").unwrap(), 42);
        assert_eq!(
            parse_topic_id("https://example.com/t/1").unwrap_err().code,
            "BAD_INPUT"
        );
        assert_eq!(parse_topic_id("not-a-topic").unwrap_err().code, "BAD_INPUT");
    }

    #[test]
    fn topic_items_follow_the_unified_shape() {
        let topic = json!({
            "id": 1,
            "title": "T",
            "url": "https://www.v2ex.com/t/1",
            "created": 1_700_000_000,
            "content": "hello",
            "member": {"username": "alice"}
        });
        let item = topic_item(&topic, None, None);
        assert_eq!(item["title"], "T");
        assert_eq!(item["author"], "alice");
        assert_eq!(item["published_at"], "2023-11-14T22:13:20Z");
        assert_eq!(item["text"], "hello");
        assert_eq!(item["raw"]["id"], 1);
    }

    #[test]
    fn query_encoding_matches_python() {
        assert_eq!(
            api_url(
                "https://v.test",
                "/api/x",
                &[("node_name", "py thon".into())]
            ),
            "https://v.test/api/x?node_name=py+thon"
        );
    }
}
