//! Native backends against local TCP fixtures: request shape, item shape and
//! error mapping, with no live network dependency.

use reach_channels::http::Endpoints;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Response = (u16, Vec<u8>, Vec<(String, String)>);
type Handler = dyn Fn(&str, &str) -> Response + Send + Sync;

/// A canned 200 response.
fn ok(body: impl AsRef<[u8]>) -> Response {
    (200, body.as_ref().to_vec(), Vec::new())
}

/// A canned non-200 response.
fn failed(status: u16, body: impl AsRef<[u8]>) -> Response {
    (status, body.as_ref().to_vec(), Vec::new())
}

/// A canned 200 JSON response.
fn json_ok(value: Value) -> Response {
    ok(value.to_string())
}

struct Fixture {
    base: String,
}

/// Serve canned responses from a loopback port; `handler(method, path)` answers.
async fn spawn(handler: impl Fn(&str, &str) -> Response + Send + Sync + 'static) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let handler: Arc<Handler> = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65536];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let request_line = text.lines().next().unwrap_or_default().to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("GET").to_string();
                let path = parts.next().unwrap_or("/").to_string();
                let (status, body, headers) = handler(&method, &path);
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    403 => "Forbidden",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    _ => "Internal Server Error",
                };
                let mut head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Fixture {
        base: format!("http://{address}"),
    }
}

fn endpoints(v2ex: &str, github: &str, tencent_quote: &str, tencent_search: &str) -> Endpoints {
    Endpoints {
        v2ex: v2ex.into(),
        github_api: github.into(),
        tencent_quote: tencent_quote.into(),
        tencent_search: tencent_search.into(),
        ..Endpoints::default()
    }
}

fn temp_state(config: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp state");
    if let Some(text) = config {
        std::fs::write(dir.path().join("config.yaml"), text).expect("write config");
    }
    dir
}

async fn call(
    channel: &str,
    action: &str,
    backend: &str,
    params: Value,
    endpoints: &Endpoints,
    state: &std::path::Path,
) -> reach_core::Result<Vec<Value>> {
    reach_channels::execute_with(
        channel,
        action,
        backend,
        &params,
        std::time::Duration::from_secs(5),
        state,
        endpoints,
    )
    .await
}

#[tokio::test]
async fn v2ex_hot_returns_unified_items() {
    let fixture = spawn(|_method, path| {
        assert_eq!(path, "/api/topics/hot.json");
        json_ok(json!([
            {"id": 1, "title": "First", "url": "https://www.v2ex.com/t/1", "created": 1_700_000_000,
             "content": "body one", "member": {"username": "alice"}},
            {"id": 2, "title": "Second", "url": "https://www.v2ex.com/t/2", "created": 1_700_000_100,
             "member": {"username": "bob"}}
        ]))
    })
    .await;
    let state = temp_state(None);
    let items = call(
        "v2ex",
        "hot",
        "v2ex-api",
        json!({"limit": 2}),
        &endpoints(&fixture.base, "", "", ""),
        state.path(),
    )
    .await
    .expect("v2ex hot");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["title"], "First");
    assert_eq!(items[0]["author"], "alice");
    assert_eq!(items[0]["published_at"], "2023-11-14T22:13:20Z");
    assert_eq!(items[1]["text"], Value::Null);
    let keys: Vec<&String> = items[0].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["title", "url", "author", "published_at", "text", "raw"]
    );
}

#[tokio::test]
async fn v2ex_node_reports_not_found_and_read_joins_replies() {
    let fixture = spawn(|_method, path| {
        if path.starts_with("/api/topics/show.json?node_name=missing") {
            return json_ok(json!({"message": "node not found"}));
        }
        if path.starts_with("/api/topics/show.json?id=1000") {
            return json_ok(json!([{"id": 1000, "title": "Topic",
                                   "url": "https://www.v2ex.com/t/1000", "created": 1_700_000_000,
                                   "content": "hello", "member": {"username": "alice"}}]));
        }
        if path.starts_with("/api/replies/show.json?topic_id=1000") {
            return json_ok(json!([{"content": "hi back", "member": {"username": "bob"}}]));
        }
        failed(404, "{}")
    })
    .await;
    let state = temp_state(None);
    let endpoints = endpoints(&fixture.base, "", "", "");

    let error = call(
        "v2ex",
        "node",
        "v2ex-api",
        json!({"node_name": "missing", "limit": 3}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NOT_FOUND");
    assert!(error.message.contains("V2EX node not found: missing"));

    let items = call(
        "v2ex",
        "read",
        "v2ex-api",
        json!({"target": "https://www.v2ex.com/t/1000"}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("v2ex read");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "hello\n\n@bob: hi back");
    assert_eq!(items[0]["raw"]["topic"]["id"], 1000);
    assert_eq!(items[0]["raw"]["replies"][0]["content"], "hi back");
}

#[tokio::test]
async fn github_search_uses_the_rest_shape() {
    let fixture = spawn(|_method, path| {
        assert!(
            path.starts_with("/search/repositories?q=yt-dlp&per_page=2"),
            "{path}"
        );
        json_ok(json!({"items": [
            {"full_name": "yt-dlp/yt-dlp", "html_url": "https://github.com/yt-dlp/yt-dlp",
             "owner": {"login": "yt-dlp"}, "created_at": "2020-01-01T00:00:00Z",
             "description": "downloader", "stargazers_count": 1, "license": {"spdx_id": "Unlicense"}}
        ]}))
    })
    .await;
    let state = temp_state(None);
    let items = call(
        "github",
        "search",
        "github-api",
        json!({"query": "yt-dlp", "limit": 2}),
        &endpoints("", &fixture.base, "", ""),
        state.path(),
    )
    .await
    .expect("github search");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "yt-dlp/yt-dlp");
    assert_eq!(items[0]["raw"]["license"], "Unlicense");
}

#[tokio::test]
async fn github_classifies_403_with_and_without_rate_limit_evidence() {
    let denied = spawn(|_method, _path| failed(403, "{}")).await;
    let state = temp_state(None);
    let error = call(
        "github",
        "read",
        "github-api",
        json!({"target": "owner/repo"}),
        &endpoints("", &denied.base, "", ""),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
    assert!(
        error.message.contains("no rate-limit evidence"),
        "{}",
        error.message
    );

    let exhausted = spawn(|_method, _path| {
        (
            403,
            b"{}".to_vec(),
            vec![("X-RateLimit-Remaining".into(), "0".into())],
        )
    })
    .await;
    let error = call(
        "github",
        "read",
        "github-api",
        json!({"target": "owner/repo"}),
        &endpoints("", &exhausted.base, "", ""),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "RATE_LIMITED");

    let retry_after = spawn(|_method, _path| {
        (
            403,
            b"{}".to_vec(),
            vec![("Retry-After".into(), "30".into())],
        )
    })
    .await;
    let error = call(
        "github",
        "read",
        "github-api",
        json!({"target": "owner/repo"}),
        &endpoints("", &retry_after.base, "", ""),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "RATE_LIMITED");

    let missing = spawn(|_method, _path| failed(404, "{}")).await;
    let error = call(
        "github",
        "read",
        "github-api",
        json!({"target": "owner/repo"}),
        &endpoints("", &missing.base, "", ""),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NOT_FOUND");
}

#[tokio::test]
async fn xueqiu_tencent_quote_decodes_gbk_names() {
    // Serve real GBK bytes, as qt.gtimg.cn does.
    // Field layout of a real qt.gtimg.cn row: 30 = time, 31 = change,
    // 32 = percent, 33 = high, 34 = low.
    let filler: Vec<String> = (0..24).map(|index| index.to_string()).collect();
    let body = format!(
        "v_sh600519=\"1~贵州茅台~600519~1700.00~1690.00~1695.00~{}~20241114150000~10.00~0.59~1710~1680~1688\";",
        filler.join("~")
    );
    let (encoded, _, _) = encoding_rs::GBK.encode(&body);
    let payload: Vec<u8> = encoded.into_owned();
    let fixture = spawn(move |_method, path| {
        assert_eq!(path, "/q=sh600519");
        ok(&payload)
    })
    .await;
    let state = temp_state(None);
    let items = call(
        "xueqiu",
        "quote",
        "tencent",
        json!({"symbol": "SH600519"}),
        &endpoints("", "", &fixture.base, ""),
        state.path(),
    )
    .await
    .expect("tencent quote");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "贵州茅台 (SH600519)");
    assert_eq!(items[0]["url"], "https://xueqiu.com/S/SH600519");
    assert_eq!(items[0]["text"], "贵州茅台 1700.00 (0.59%)");
    assert_eq!(items[0]["raw"]["current"], "1700.00");
    assert_eq!(items[0]["raw"]["percent"], "0.59");
    assert_eq!(items[0]["raw"]["symbol"], "SH600519");
}

#[tokio::test]
async fn xueqiu_tencent_quote_is_not_found_when_the_payload_is_short() {
    let fixture = spawn(|_method, _path| ok("v_sh600519=\"\";")).await;
    let state = temp_state(None);
    let error = call(
        "xueqiu",
        "quote",
        "tencent",
        json!({"symbol": "SH600519"}),
        &endpoints("", "", &fixture.base, ""),
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NOT_FOUND");
}

#[tokio::test]
async fn xueqiu_search_parses_tencent_rows() {
    let fixture = spawn(|_method, path| {
        assert!(path.starts_with("/s3/?v=2&q="), "{path}");
        ok("v_hint=\"sh~600519~Kweichow~GP-A~1~x^sz~000001~PingAn~GP-A~2~y\"")
    })
    .await;
    let state = temp_state(None);
    let items = call(
        "xueqiu",
        "search",
        "tencent",
        json!({"query": "600519", "limit": 2}),
        &endpoints("", "", "", &fixture.base),
        state.path(),
    )
    .await
    .expect("tencent search");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["title"], "Kweichow (SH600519)");
    assert_eq!(items[0]["raw"]["market"], "sh");
    assert_eq!(items[1]["title"], "PingAn (SZ000001)");
}

#[tokio::test]
async fn xueqiu_api_uses_the_configured_cookie_and_maps_login_errors() {
    let fixture = spawn(|_method, path| match path {
        path if path.starts_with("/v5/stock/quote.json?symbol=SH600519") => json_ok(json!({
            "data": {"quote": {"name": "贵州茅台", "symbol": "SH600519", "current": 1700, "percent": 0.59}}
        })),
        path if path.starts_with("/stock/search.json") => {
            json_ok(json!({"stocks": [{"name": "贵州茅台", "code": "SH600519"}]}))
        }
        path if path.starts_with("/v5/stock/hot_stock/list.json") => {
            failed(400, json!({"error_code": "400016"}).to_string())
        }
        _ => failed(404, "{}"),
    })
    .await;
    let state = temp_state(Some("xueqiu_cookie: xq_a_token=secret; acw_tc=1\n"));
    let endpoints = Endpoints {
        xueqiu: fixture.base.clone(),
        xueqiu_stock: fixture.base.clone(),
        ..Endpoints::default()
    };

    let items = call(
        "xueqiu",
        "quote",
        "xueqiu-api",
        json!({"symbol": "SH600519"}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("xueqiu quote");
    assert_eq!(items[0]["title"], "贵州茅台 (SH600519)");
    assert_eq!(items[0]["url"], "https://xueqiu.com/S/SH600519");

    let items = call(
        "xueqiu",
        "search",
        "xueqiu-api",
        json!({"query": "茅台", "limit": 3}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("xueqiu search");
    assert_eq!(items[0]["title"], "贵州茅台 (SH600519)");

    let error = call(
        "xueqiu",
        "hot_stocks",
        "xueqiu-api",
        json!({"limit": 3}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");
    assert!(error.hint.contains("login xueqiu"));
}

#[tokio::test]
async fn xueqiu_community_actions_need_login_without_a_cookie() {
    // The homepage bootstraps no session cookie, so a community read cannot
    // pretend to have one.
    let fixture = spawn(|_method, _path| json_ok(json!({"list": []}))).await;
    let state = temp_state(None);
    let endpoints = Endpoints {
        xueqiu: fixture.base.clone(),
        xueqiu_stock: fixture.base.clone(),
        xueqiu_home: fixture.base.clone(),
        ..Endpoints::default()
    };
    let error = call(
        "xueqiu",
        "hot_posts",
        "xueqiu-api",
        json!({"limit": 3}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_LOGIN");
}

#[tokio::test]
async fn bilibili_public_api_reads_videos_and_maps_codes() {
    let fixture = spawn(|_method, path| {
        if path.starts_with("/x/web-interface/view?bvid=BV1GJ411x7h7") {
            return json_ok(json!({"code": 0, "data": {
                "bvid": "BV1GJ411x7h7", "aid": 1, "title": "Video", "desc": "D",
                "pubdate": 1_700_000_000, "owner": {"name": "up"},
                "duration": 12, "stat": {"view": 5}, "tname": "tech"}}));
        }
        if path.starts_with("/x/web-interface/view?bvid=BV0000000000") {
            return json_ok(json!({"code": -404, "message": "nope"}));
        }
        if path.starts_with("/x/web-interface/popular?ps=2&pn=1") {
            return json_ok(json!({"code": 0, "data": {"list": [
                {"bvid": "BV1", "title": "A", "owner": {"name": "u"}, "pubdate": 1},
                {"bvid": "BV2", "title": "B", "owner": {"name": "u"}, "pubdate": 2}
            ]}}));
        }
        ok("<html>verify</html>")
    })
    .await;
    let state = temp_state(None);
    let endpoints = Endpoints {
        bilibili_api: fixture.base.clone(),
        ..Endpoints::default()
    };

    let items = call(
        "bilibili",
        "read",
        "bilibili-api",
        json!({"target": "BV1GJ411x7h7"}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("bilibili read");
    assert_eq!(items[0]["title"], "Video");
    assert_eq!(items[0]["author"], "up");
    assert_eq!(
        items[0]["url"],
        "https://www.bilibili.com/video/BV1GJ411x7h7"
    );
    assert_eq!(items[0]["published_at"], "2023-11-14T22:13:20Z");
    assert_eq!(items[0]["raw"]["tname"], "tech");

    let error = call(
        "bilibili",
        "read",
        "bilibili-api",
        json!({"target": "BV0000000000"}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NOT_FOUND");
    assert_eq!(error.message, "video not found");

    let items = call(
        "bilibili",
        "hot",
        "bilibili-api",
        json!({"limit": 2}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("bilibili hot");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["title"], "A");

    // An unparsable body is a verification page, never an empty result.
    let error = call(
        "bilibili",
        "hot",
        "bilibili-api",
        json!({"limit": 3}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
    assert!(error.message.contains("verification page"));
}

#[tokio::test]
async fn web_jina_read_parses_and_rejects_challenge_pages() {
    let fixture = spawn(|_method, path| {
        if path.contains("challenge") {
            // A real Jina anti-bot page: captcha warning *and* challenge marker.
            return ok("Warning: this request is requiring captcha verification\nTitle: Just a moment...\n");
        }
        ok("Title: Example\n\nMarkdown Content:\nhello world")
    })
    .await;
    let state = temp_state(None);
    let endpoints = Endpoints {
        jina: fixture.base.clone(),
        ..Endpoints::default()
    };
    let items = call(
        "web",
        "read",
        "jina",
        json!({"target": "https://example.com/page"}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("jina read");
    assert_eq!(items[0]["title"], "Example");
    assert_eq!(items[0]["text"], "hello world");
    assert_eq!(items[0]["url"], "https://example.com/page");

    let error = call(
        "web",
        "read",
        "jina",
        json!({"target": "https://example.com/challenge"}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
    assert!(error.message.contains("anti-bot"));
}

#[tokio::test]
async fn web_exa_search_parses_sse_frames_and_errors() {
    let fixture = spawn(|_method, path| {
        assert_eq!(path, "/mcp");
        let frame = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"content": [{"type": "text", "text":
                "Title: One\nURL: https://a.test/1\nPublished: 2024-01-01\nAuthor: Ada\nBody one"}],
                "isError": false}
        });
        ok(format!("event: message\ndata: {frame}\n\n"))
    })
    .await;
    let state = temp_state(None);
    let endpoints = Endpoints {
        exa_mcp: format!("{}/mcp", fixture.base),
        ..Endpoints::default()
    };
    let items = call(
        "web",
        "search",
        "exa",
        json!({"query": "rust", "limit": 1}),
        &endpoints,
        state.path(),
    )
    .await
    .expect("exa search");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "One");
    assert_eq!(items[0]["author"], "Ada");
    assert_eq!(items[0]["published_at"], "2024-01-01");

    let failing = spawn(|_method, _path| {
        let frame = json!({"jsonrpc": "2.0", "id": 1, "result": {"content": [], "isError": true}});
        ok(format!("data: {frame}\n\n"))
    })
    .await;
    let endpoints = Endpoints {
        exa_mcp: format!("{}/mcp", failing.base),
        ..Endpoints::default()
    };
    let error = call(
        "web",
        "search",
        "exa",
        json!({"query": "rust", "limit": 1}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UPSTREAM_BROKEN");
}

#[tokio::test]
async fn unknown_channel_action_and_backend_are_bad_input() {
    let state = temp_state(None);
    let endpoints = Endpoints::default();
    let error = call("nope", "hot", "jina", json!({}), &endpoints, state.path())
        .await
        .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
    assert!(error.message.contains("unknown channel"));

    let error = call(
        "v2ex",
        "nope",
        "v2ex-api",
        json!({}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
    assert!(error.message.contains("unknown action"));

    let error = call("v2ex", "hot", "jina", json!({}), &endpoints, state.path())
        .await
        .unwrap_err();
    assert_eq!(error.code, "BAD_INPUT");
    assert!(error.message.contains("not a backend"));
}

#[tokio::test]
async fn external_tool_backends_report_need_setup() {
    let state = temp_state(None);
    let endpoints = Endpoints::default();
    // bili-cli and boss-agent-cli now execute the real tools; opencli still
    // requires the explicitly bound dedicated browser, which this state lacks.
    let error = call(
        "twitter",
        "search",
        "opencli",
        json!({"query": "rust", "limit": 1}),
        &endpoints,
        state.path(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_SETUP");
}
