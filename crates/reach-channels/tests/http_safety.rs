use reach_channels::{http, urlsec};
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
#[test]
fn url_validation_preserves_the_upstream_output_spelling() {
    for (input, expected) in [
        ("https://example.com", "https://example.com"),
        ("example.com", "https://example.com"),
        (
            "HTTP://EXAMPLE.COM:80/a/../b?x=1#c",
            "http://EXAMPLE.COM:80/a/../b?x=1#c",
        ),
    ] {
        assert_eq!(urlsec::normalize_public_http_url(input).unwrap(), expected);
    }
    for input in ["https://@example.com", "https://%65xample.com"] {
        assert!(
            urlsec::normalize_public_http_url(input).is_err(),
            "ambiguous authority must fail: {input}"
        );
    }
}
async fn oversized() -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut b = [0u8; 4096];
        let _ = s.read(&mut b).await;
        s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n01234567890123456789012345678901\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
    });
    (format!("http://{addr}/"), task)
}
#[tokio::test]
async fn body_cap_rejects_before_waiting_for_the_entire_response() {
    for kind in ["text", "bytes", "post", "raw"] {
        let (url, task) = oversized().await;
        let start = Instant::now();
        let budget = Duration::from_millis(500);
        let result = match kind {
            "text" => http::get_text(&url, budget, &[], 8).await.map(|_| ()),
            "bytes" => http::get_bytes(&url, budget, &[], 8).await.map(|_| ()),
            "post" => http::post_json_text(&url, &serde_json::json!({}), budget, &[], 8)
                .await
                .map(|_| ()),
            _ => http::get_raw(&url, budget, &[], 8).await.map(|_| ()),
        };
        task.abort();
        let error = result.unwrap_err();
        assert_eq!(error.code, "UPSTREAM_BROKEN", "{kind}: {error}");
        assert!(error.message.contains("exceeds"));
        assert!(start.elapsed() < budget);
    }
}
#[tokio::test]
async fn http_error_matches_python_reason_without_echoing_html() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await;
        let body = "<html>upstream-private-diagnostic</html>";
        let response=format!("HTTP/1.1 412 Precondition Failed\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        s.write_all(response.as_bytes()).await.unwrap();
    });
    let e = http::get_text(
        &format!("http://{addr}/"),
        Duration::from_secs(2),
        &[],
        1024,
    )
    .await
    .unwrap_err();
    assert_eq!(e.message, "HTTP 412: Precondition Failed");
}
