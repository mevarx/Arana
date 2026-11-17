//! Live network checks for the native no-auth backends.
//!
//! `#[ignore]` by default: CI must not depend on third-party uptime. Run with
//!
//!     cargo test -p reach-channels --test live_smoke -- --ignored --nocapture
//!
//! `live5_summary_writes_artifact` additionally writes
//! `artifacts/smoke/summary.json` with what each probe actually returned, so a
//! platform block is recorded instead of silently turning into a pass.

use reach_channels::http::Endpoints;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/smoke")
}

async fn probe(channel: &str, action: &str, backend: &str, params: Value) -> (Value, Duration) {
    let dir = tempfile::tempdir().expect("temp state");
    let started = Instant::now();
    let outcome = reach_channels::execute(
        channel,
        action,
        backend,
        &params,
        Duration::from_secs(30),
        dir.path(),
    )
    .await;
    let elapsed = started.elapsed();
    let value = match outcome {
        Ok(items) => json!({
            "ok": true,
            "item_count": items.len(),
            "first_title": items.first().and_then(|item| item["title"].as_str()),
            "first_url": items.first().and_then(|item| item["url"].as_str()),
        }),
        Err(error) => json!({
            "ok": false,
            "error_code": error.code,
            "error_message": error.message,
        }),
    };
    (value, elapsed)
}

fn assert_items(items: &[Value]) {
    assert!(!items.is_empty(), "expected at least one item");
    for item in items {
        let object = item.as_object().expect("item is an object");
        assert_eq!(object.len(), 6, "item must have exactly 6 fields");
        for key in ["title", "url", "author", "published_at", "text", "raw"] {
            assert!(object.contains_key(key), "item is missing {key}");
        }
    }
}

async fn run(channel: &str, action: &str, backend: &str, params: Value) -> Vec<Value> {
    let dir = tempfile::tempdir().expect("temp state");
    match reach_channels::execute(
        channel,
        action,
        backend,
        &params,
        Duration::from_secs(30),
        dir.path(),
    )
    .await
    {
        Ok(items) => {
            println!(
                "{channel}/{action}/{backend}: {} items, first title {:?}",
                items.len(),
                items.first().and_then(|item| item["title"].as_str())
            );
            items
        }
        Err(error) => panic!("{channel}/{action}/{backend} failed: {error}"),
    }
}

#[tokio::test]
#[ignore = "live network"]
async fn v2ex_live() {
    assert_items(&run("v2ex", "hot", "v2ex-api", json!({"limit": 3})).await);
    let items = run(
        "v2ex",
        "read",
        "v2ex-api",
        json!({"target": "https://www.v2ex.com/t/1000"}),
    )
    .await;
    assert_items(&items);
    assert!(items[0]["text"].as_str().unwrap_or_default().len() > 20);
    assert_items(&run("v2ex", "user", "v2ex-api", json!({"username": "Livid"})).await);
}

#[tokio::test]
#[ignore = "live network"]
async fn github_live_rest() {
    let items = run(
        "github",
        "search",
        "github-api",
        json!({"query": "yt-dlp", "limit": 2}),
    )
    .await;
    assert_items(&items);
    assert!(items[0]["url"]
        .as_str()
        .unwrap_or_default()
        .contains("github.com/"));
    // Unauthenticated REST allows 60 requests/hour per IP; a spent quota is a
    // shared-environment condition, so record it instead of failing the probe.
    let (result, _) = probe(
        "github",
        "read",
        "github-api",
        json!({"target": "yt-dlp/yt-dlp"}),
    )
    .await;
    println!("github/read: {result}");
    assert!(
        result["ok"] == true || result["error_code"] == "RATE_LIMITED",
        "unexpected github read outcome: {result}"
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn xueqiu_live_quote_and_search() {
    let items = run("xueqiu", "quote", "tencent", json!({"symbol": "SH600519"})).await;
    assert_items(&items);
    println!("  quote: {}", items[0]["text"]);
    assert_items(
        &run(
            "xueqiu",
            "search",
            "tencent",
            json!({"query": "茅台", "limit": 3}),
        )
        .await,
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn bilibili_live_public_api() {
    // Bilibili risk-controls the video-detail endpoint per client and IP; a
    // block is a real platform answer, not a satisfying read.
    let (result, _) = probe(
        "bilibili",
        "read",
        "bilibili-api",
        json!({"target": "BV1GJ411x7h7"}),
    )
    .await;
    println!("bilibili/read: {result}");
    assert!(
        result["ok"] == true
            || [
                "NOT_FOUND",
                "UPSTREAM_BROKEN",
                "PLATFORM_BLOCKED",
                "RATE_LIMITED"
            ]
            .contains(&result["error_code"].as_str().unwrap_or("")),
        "unexpected bilibili read outcome: {result}"
    );
    let items = run("bilibili", "hot", "bilibili-api", json!({"limit": 3})).await;
    assert_items(&items);
    assert_eq!(items.len(), 3);
}

#[tokio::test]
#[ignore = "live network"]
async fn web_live_jina_and_exa() {
    let items = run(
        "web",
        "read",
        "jina",
        json!({"target": "https://example.com"}),
    )
    .await;
    assert_items(&items);
    assert!(items[0]["text"].as_str().unwrap_or_default().len() > 20);
    assert_items(
        &run(
            "web",
            "search",
            "exa",
            json!({"query": "yt-dlp github repository", "limit": 2}),
        )
        .await,
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn github_gh_cli_backend_live() {
    // The `gh` backend is declared first upstream; when gh has no credentials it
    // must report NEED_SETUP so dispatch falls through to github-api.
    let (result, _) = probe(
        "github",
        "search",
        "gh",
        json!({"query": "yt-dlp", "limit": 2}),
    )
    .await;
    println!("github/search/gh: {result}");
    assert!(
        result["ok"] == true
            || ["NEED_SETUP", "RATE_LIMITED", "UPSTREAM_BROKEN"]
                .contains(&result["error_code"].as_str().unwrap_or("")),
        "unexpected gh outcome: {result}"
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn live5_summary_writes_artifact() {
    let probes = [
        ("v2ex:hot", "v2ex", "hot", "v2ex-api", json!({"limit": 3})),
        (
            "github:search",
            "github",
            "search",
            "github-api",
            json!({"query": "yt-dlp", "limit": 2}),
        ),
        (
            "xueqiu:quote",
            "xueqiu",
            "quote",
            "tencent",
            json!({"symbol": "SH600519"}),
        ),
        (
            "bilibili:hot",
            "bilibili",
            "hot",
            "bilibili-api",
            json!({"limit": 3}),
        ),
        (
            "web:read",
            "web",
            "read",
            "jina",
            json!({"target": "https://example.com"}),
        ),
    ];
    let mut rows = Vec::new();
    for (label, channel, action, backend, params) in probes {
        let (outcome, elapsed) = probe(channel, action, backend, params.clone()).await;
        println!("{label}: {outcome} in {}ms", elapsed.as_millis());
        rows.push(json!({
            "probe": label,
            "channel": channel,
            "action": action,
            "backend": backend,
            "params": params,
            "elapsed_ms": elapsed.as_millis() as u64,
            "outcome": outcome,
        }));
    }
    let succeeded = rows
        .iter()
        .filter(|row| row["outcome"]["ok"] == true)
        .count();
    let defaults = Endpoints::default();
    let summary = json!({
        "schema_version": 1,
        "kind": "reach-channels-native-live-probes",
        "recorded_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "probe_command": "cargo test -p reach-channels --test live_smoke -- --ignored --nocapture",
        "endpoints": {
            "v2ex": defaults.v2ex,
            "github_api": defaults.github_api,
            "tencent_quote": defaults.tencent_quote,
            "bilibili_api": defaults.bilibili_api,
            "jina": defaults.jina,
        },
        "probe_count": rows.len(),
        "succeeded": succeeded,
        "failed": rows.len() - succeeded,
        "probes": rows,
    });
    let dir = artifact_dir();
    std::fs::create_dir_all(&dir).expect("artifact dir");
    std::fs::write(
        dir.join("summary.json"),
        serde_json::to_string_pretty(&summary).expect("serialize") + "\n",
    )
    .expect("write summary");
    println!("wrote {}", dir.join("summary.json").display());
    assert!(
        succeeded >= 3,
        "at least three native probes must succeed: {summary}"
    );
}
