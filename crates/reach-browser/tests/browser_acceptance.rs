use futures_util::{SinkExt, StreamExt};
use reach_browser::{login, manage, watch_with_config, SlotConfig, Slots, WatchConfig};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn command(ws: &str, method: &str, params: Value) -> Value {
    let (mut socket, _) = connect_async(ws).await.unwrap();
    socket
        .send(Message::Text(
            json!({"id":1,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["id"] == 1 {
                assert!(v["error"].is_null(), "CDP {method}: {v}");
                return v["result"].clone();
            }
        }
        panic!("CDP closed before reply")
    })
    .await
    .unwrap()
}
async fn fixture(marker: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4096];
                let _ = socket.read(&mut b).await;
                let body = format!(
                    "<!doctype html><html><title>{marker}</title><body>{marker}</body></html>"
                );
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (port, task)
}
async fn page(port: u16, ws: &str, url: &str, marker: &str, site: &str, state: &Path) -> String {
    let slots = Slots::new(state.join("browser-locks"), SlotConfig::default());
    let _slot = slots.acquire(site).await.unwrap();
    let created = command(ws, "Target.createTarget", json!({"url":url})).await;
    let id = created["targetId"].as_str().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let target_ws = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let targets: Value = client
                .get(format!("http://127.0.0.1:{port}/json/list"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(target) = targets.as_array().unwrap().iter().find(|t| t["id"] == id) {
                if let Some(ws) = target["webSocketDebuggerUrl"].as_str() {
                    break ws.to_owned();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let marker = serde_json::to_string(marker).unwrap();
    let expression=format!("new Promise(resolve => {{ const tick = () => {{if (document.body && document.body.innerText.includes({marker})) resolve(document.body.innerText); else setTimeout(tick, 20);}};tick();}})");
    let result = command(
        &target_ws,
        "Runtime.evaluate",
        json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
    )
    .await;
    let text = result["result"]["value"].as_str().unwrap().to_owned();
    command(ws, "Target.closeTarget", json!({"targetId":id})).await;
    text
}
#[tokio::test]
#[ignore = "Launches real dedicated Chromium and a visible credential-free test window"]
async fn parallel_browser_reads_and_actual_login_pin_survive_idle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let state = tempfile::tempdir().unwrap();
    reach_core::state::atomic_write(
        &state.path().join("config.yaml"),
        format!("browser_port: {port}\n").as_bytes(),
    )
    .unwrap();
    let started = manage("setup", &[], state.path()).await.unwrap();
    assert_eq!(started["owned"], true);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let version: Value = client
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ws = version["webSocketDebuggerUrl"].as_str().unwrap();
    let (a, server_a) = fixture("ARANA_PARALLEL_SITE_A").await;
    let (b, server_b) = fixture("ARANA_PARALLEL_SITE_B").await;
    let url_a = format!("http://127.0.0.1:{a}/");
    let url_b = format!("http://localhost:{b}/");
    let (text_a, text_b) = tokio::join!(
        page(
            port,
            ws,
            &url_a,
            "ARANA_PARALLEL_SITE_A",
            "a.example",
            state.path()
        ),
        page(
            port,
            ws,
            &url_b,
            "ARANA_PARALLEL_SITE_B",
            "b.example",
            state.path()
        )
    );
    server_a.abort();
    server_b.abort();
    manage("stop", &[], state.path()).await.unwrap();
    // No login form or credentials: exercise the same real visible-window handoff.
    let visible = login("m3-test", "https://example.com", state.path())
        .await
        .unwrap();
    let watcher_state = state.path().to_path_buf();
    let watcher = tokio::spawn(async move {
        watch_with_config(
            &watcher_state,
            WatchConfig {
                idle_after: Duration::ZERO,
                poll_every: Duration::from_millis(20),
            },
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pinned = manage("status", &[], state.path()).await.unwrap();
    std::fs::remove_file(state.path().join("browser/login-pin.json")).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(20), watcher).await;
    let final_status = manage("status", &[], state.path()).await.unwrap();
    let _ = manage("stop", &[], state.path()).await;
    assert!(text_a.contains("ARANA_PARALLEL_SITE_A"));
    assert!(text_b.contains("ARANA_PARALLEL_SITE_B"));
    assert_eq!(visible["headless"], false);
    assert_eq!(pinned["login_pinned"], true);
    assert_eq!(pinned["running"], true);
    assert!(closed.unwrap().unwrap().is_ok());
    assert_eq!(final_status["running"], false);
    println!("{{\"parallel_real_cdp_reads\":2,\"visible_window_pinned\":true,\"survived_zero_idle_limit\":true,\"closed_after_unpin\":true}}");
}
