use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn endpoint(port: u16, path: &str) -> reach_core::Result<Value> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(reach_core::ReachError::from)?;
    client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(reach_core::ReachError::from)?
        .json()
        .await
        .map_err(reach_core::ReachError::from)
}
pub async fn targets(port: u16) -> reach_core::Result<Vec<Value>> {
    Ok(endpoint(port, "/json/list")
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default())
}
pub async fn call(
    port: u16,
    method: &str,
    params: Value,
    target_id: Option<&str>,
) -> reach_core::Result<Value> {
    let ws_url = if let Some(id) = target_id {
        targets(port)
            .await?
            .into_iter()
            .find(|t| t["id"] == id)
            .and_then(|t| t["webSocketDebuggerUrl"].as_str().map(str::to_owned))
    } else {
        endpoint(port, "/json/version").await?["webSocketDebuggerUrl"]
            .as_str()
            .map(str::to_owned)
    }
    .ok_or_else(|| {
        reach_core::ReachError::new(
            "UPSTREAM_BROKEN",
            "No dedicated Chromium CDP websocket target",
        )
    })?;
    let parsed = url::Url::parse(&ws_url)
        .map_err(|e| reach_core::ReachError::new("UPSTREAM_BROKEN", e.to_string()))?;
    if parsed.scheme() != "ws"
        || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        || parsed.port_or_known_default() != Some(port)
    {
        return Err(reach_core::ReachError::new(
            "UPSTREAM_BROKEN",
            "Refusing a non-loopback or unexpected CDP websocket target",
        ));
    }
    let (mut socket, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), connect_async(&ws_url))
            .await
            .map_err(|_| reach_core::ReachError::new("TIMEOUT", "CDP websocket connect timed out"))?
            .map_err(|e| {
                reach_core::ReachError::new("UPSTREAM_BROKEN", format!("CDP connect failed: {e}"))
            })?;
    let id = 1;
    socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|e| reach_core::ReachError::new("UPSTREAM_BROKEN", e.to_string()))?;
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(15), socket.next())
            .await
            .map_err(|_| {
                reach_core::ReachError::new("TIMEOUT", "CDP command response timed out")
            })?;
        let Some(frame) = next else {
            break;
        };
        let frame =
            frame.map_err(|e| reach_core::ReachError::new("UPSTREAM_BROKEN", e.to_string()))?;
        if let Message::Text(text) = frame {
            let msg: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if msg["id"] == id {
                if !msg["error"].is_null() {
                    return Err(reach_core::ReachError::new(
                        "UPSTREAM_BROKEN",
                        msg["error"]["message"]
                            .as_str()
                            .unwrap_or("CDP command failed"),
                    ));
                }
                return Ok(msg.get("result").cloned().unwrap_or(json!({})));
            }
        }
    }
    Err(reach_core::ReachError::new(
        "UPSTREAM_BROKEN",
        "CDP websocket closed without a reply",
    ))
}
pub async fn cookies(port: u16) -> reach_core::Result<Vec<Value>> {
    Ok(
        call(port, "Storage.getCookies", json!({}), None).await?["cookies"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    )
}
