//! SSRF corpus — ported from upstream `tests/test_transcribe.py` +
//! `tests/test_url_security.py` plus the DNS/redirect protections.

use reach_tools::ssrf::*;
use std::time::Duration;

/// The upstream literal-IP grammar corpus: every shorthand that reaches a
/// private/internal address under the C resolver's `inet_aton` rules.
#[test]
fn literal_ip_parses_inet_aton_shorthands() {
    let cases: &[(&str, &str)] = &[
        ("127.1", "127.0.0.1"),
        ("127.0.1", "127.0.0.1"),
        ("2130706433", "127.0.0.1"),
        ("0x7f000001", "127.0.0.1"),
        ("0177.0.0.1", "127.0.0.1"),
        ("017700000001", "127.0.0.1"),
        ("0", "0.0.0.0"),
        ("192.168.1", "192.168.0.1"),
        ("2852039166", "169.254.169.254"),
        ("0xA9FEA9FE", "169.254.169.254"),
        ("0xA9.0xFE.0xA9.0xFE", "169.254.169.254"),
    ];
    for (host, expect) in cases {
        let ip = literal_ip(host).unwrap_or_else(|| panic!("{host} did not parse"));
        assert_eq!(ip.to_string(), *expect, "host {host}");
    }
    // Canonical dotted-quad and IPv6 forms parse too.
    assert_eq!(literal_ip("8.8.8.8").unwrap().to_string(), "8.8.8.8");
    assert_eq!(literal_ip("::1").unwrap().to_string(), "::1");
    // A real hostname is not a literal.
    assert!(literal_ip("example.com").is_none());
    assert!(literal_ip("1.2.3.4.5").is_none());
}

#[test]
fn private_ip_covers_loopback_linklocal_reserved_and_shorthands() {
    for host in [
        "127.0.0.1",
        "127.1",
        "2130706433",
        "0x7f000001",
        "0177.0.0.1",
        "0",
        "10.0.0.1",
        "192.168.1.1",
        "192.168.1",
        "169.254.169.254",
        "2852039166",
        "0xA9FEA9FE",
        "172.16.0.1",
        "fd00::1",
        "::1",
        "224.0.0.1",
        "255.255.255.255",
    ] {
        assert!(is_private_ip(host), "{host} must be private");
    }
    for host in ["1.1.1.1", "8.8.8.8", "010.010.010.010", "93.184.216.34"] {
        assert!(!is_private_ip(host), "{host} must be public");
    }
    // Hostnames are not literal IPs — never treated as private by this check.
    assert!(!is_private_ip("example.com"));
}

#[test]
fn assert_safe_public_url_rejects_internal_targets() {
    for url in [
        // literal shorthands of loopback and the cloud metadata endpoint
        "http://127.1/a.mp3",
        "http://127.0.1/a.mp3",
        "http://2130706433/a.mp3",
        "http://0x7f000001/a.mp3",
        "http://0177.0.0.1/a.mp3",
        "http://017700000001/a.mp3",
        "http://0/a.mp3",
        "http://192.168.1/a.mp3",
        "http://2852039166/a.mp3",
        "http://0xA9FEA9FE/a.mp3",
        // full-width digit and letter homoglyphs
        "http://１２７.０.０.１/a.mp3",
        "http://２１３０７０６４３３/a.mp3",
        "http://０x７f０００００１/a.mp3",
        "http://ⓛⓞⓒⓐⓛⓗⓞⓢⓣ/a.mp3",
        "http://ℓocalhost/a.mp3",
        // percent-encoded hosts
        "http://%31%32%37.0.0.1/a.mp3",
        "http://127%2e0%2e0%2e1/a.mp3",
        "http://local%68ost/a.mp3",
        // userinfo disguises
        "http://***@example.com/a.mp3",
        "http://user:pass@example.com/a.mp3",
        // blocked internal hostnames
        "http://localhost/a.mp3",
        "http://metadata.google.internal/latest/meta-data/",
        "http://sub.localhost/a.mp3",
        // non-http schemes
        "ftp://example.com/a.mp3",
        "file:///etc/passwd",
        // missing host
        "http:///a.mp3",
        "https://:443/a.mp3",
        // bare host with a non-numeric port
        "example.com:not-a-port/a.mp3",
    ] {
        let err = assert_safe_public_url(url)
            .err()
            .unwrap_or_else(|| panic!("{url} must be rejected"));
        let msg = format!("{} {}", err.code, err.message);
        assert!(
            msg.contains("SSRF") || msg.contains("private") || msg.contains("internal"),
            "{url} rejected with a non-SSRF error: {msg}"
        );
    }
}

#[test]
fn assert_safe_public_url_allows_public_targets() {
    for url in [
        "https://1.1.1.1/a.mp3",
        "https://8.8.8.8/a.mp3",
        // Octal dotted-quad denoting a public address.
        "http://010.010.010.010/a.mp3",
        "https://example.com/watch?v=123",
        // Bare public hosts are kept verbatim (yt-dlp supports them).
        "youtu.be/abc123",
    ] {
        let kept = assert_safe_public_url(url)
            .unwrap_or_else(|e| panic!("{url} must be allowed: {}", e.message));
        // The original URL string is what gets handed to the child process.
        assert_eq!(kept, url);
    }
}

#[test]
fn normalize_public_http_url_rejects_internal_and_local_names() {
    for url in [
        "https://localhost/x",
        "https://sub.local/x",
        "https://cache.internal/x",
        "https://printer.lan/x",
        "https://ip6-loopback/x",
        "https://host.home.arpa/x",
        "https://instance-data/x",
        "https://localdomain/x",
        "https://metadata.google.internal/x",
        "https://127.0.0.1/x",
        "https://169.254.169.254/x",
        "https://[::1]/x",
        "https://192.168.0.1/x",
        // single-label host that is not a literal IP
        "https://intranet/x",
        // userinfo and bad ports
        "https://user:pass@example.com/x",
        "https://x.com:65536/x",
        "https://x.com:not-a-port/x",
        // non-http schemes and embedded control characters (trailing
        // whitespace is trimmed before validation, as upstream does)
        "ftp://example.com/x",
        "https://exa\nmple.com/x",
        "https://example.com/x\\y",
    ] {
        assert!(
            normalize_public_http_url(url).is_err(),
            "{url} must be rejected"
        );
    }
    assert_eq!(
        normalize_public_http_url("https://Example.COM./x").unwrap(),
        "https://example.com/x"
    );
    assert!(normalize_public_http_url("example.com/x")
        .unwrap()
        .starts_with("https://example.com/x"));
}

#[test]
fn host_matches_accepts_exact_hosts_subdomains_and_ports() {
    for (url, ok) in [
        ("https://www.youtube.com/watch?v=abc", true),
        ("https://youtu.be/abc", true),
        ("https://m.youtube.com/watch?v=abc", true),
        ("https://YOUTUBE.COM/shorts/x", true),
        ("https://youtube.com:443/watch?v=abc", true),
        ("https://youtu.be:443/abc", true),
        // Real subdomains match; suffix lookalikes do not.
        ("https://youtube.com.evil.test/watch?v=abc", false),
        ("https://notyoutube.com/watch?v=abc", false),
        ("https://youtube.com@evil.test/watch?v=abc", false),
        ("https://user:pass@youtube.com/watch?v=abc", false),
        ("ftp://youtube.com/watch?v=abc", false),
        ("https://example.com", false),
        ("https://vimeo.com/123", false),
        ("", false),
        // invalid ports fail closed
        ("https://x.com:not-a-port/path", false),
        ("https://x.com:65536/path", false),
        ("https://x.com:-1/path", false),
        ("https://x.com:999999999999/path", false),
    ] {
        assert_eq!(host_matches(url, &["youtube.com", "youtu.be"]), ok, "{url}");
    }
    // Real subdomains match; suffix lookalikes do not.
    assert!(host_matches(
        "https://stock.xueqiu.com/v5/stock/quote",
        &["xueqiu.com"]
    ));
    assert!(!host_matches(
        "https://xueqiu.com.evil.test/S/SH600519",
        &["xueqiu.com"]
    ));
    assert!(host_matches(
        "https://www.xiaoyuzhoufm.com/episode/1",
        &["xiaoyuzhoufm.com"]
    ));
    assert!(host_matches(
        "https://xiaoyuzhoufm.com:443/episode/1",
        &["xiaoyuzhoufm.com"]
    ));
    assert!(!host_matches(
        "https://xiaoyuzhoufm.com.evil.test/episode/1",
        &["xiaoyuzhoufm.com"]
    ));
}

#[tokio::test]
async fn dns_check_blocks_loopback_and_private_resolutions() {
    // "localhost" resolves to loopback → rejected.
    let err = check_host_public("localhost", 80).await.unwrap_err();
    assert_eq!(err.code, "BAD_INPUT");
    // A public literal passes without any network DNS lookup.
    let addrs = check_host_public("1.1.1.1", 443).await.unwrap();
    assert!(!addrs.is_empty());
}

#[tokio::test]
async fn safe_fetch_never_connects_to_private_targets() {
    // A local server is itself a private target: the guard must fire before
    // any connection is attempted, so an empty listener is enough.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/x");
    let err = safe_fetch(&url, 1024, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(err.code, "BAD_INPUT");
    let err = safe_fetch(
        "http://169.254.169.254/latest/meta-data/",
        1024,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "BAD_INPUT");
}

#[tokio::test]
async fn fetch_following_revalidates_every_redirect_hop() {
    // Hop 1 is allow-listed by the custom validator; the redirect target is
    // not. The fetch must be rejected mid-chain, proving per-hop validation.
    let server = FixtureServer::spawn(|req| {
        if req.path == "/hop1" {
            "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        } else if req.path == "/ok" {
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_string()
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
    });
    let allowed = format!("http://127.0.0.1:{}/hop1", server.port);
    let allowed2 = format!("http://127.0.0.1:{}/ok", server.port);
    let (a1, a2) = (allowed.clone(), allowed2.clone());

    // Custom validator: allow only the two fixture paths; everything else
    // goes through the strict public-URL check (which rejects the metadata IP).
    let validator = move |url: &str| {
        if url == a1 || url == a2 {
            Ok(())
        } else {
            validate_public_url_strict(url)
        }
    };
    let err = fetch_following(&allowed, 1024, Duration::from_secs(5), &validator, false)
        .await
        .unwrap_err();
    assert_eq!(err.code, "BAD_INPUT", "redirect to private must be blocked");

    let body = fetch_following(&allowed2, 1024, Duration::from_secs(5), &validator, false)
        .await
        .unwrap();
    assert_eq!(body, b"hello");

    // The production wrapper is strict by default.
    let err = safe_fetch(&allowed, 1024, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(err.code, "BAD_INPUT");
}

fn validate_public_url_strict(url: &str) -> reach_core::Result<()> {
    assert_safe_public_url(url).map(|_| ())
}

/// Minimal blocking HTTP fixture server (one request per connection).
struct FixtureServer {
    port: u16,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn spawn(respond: impl Fn(&Req) -> String + Send + Sync + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(req) = Req::read(&mut stream) else {
                    continue;
                };
                let response = respond(&req);
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        Self {
            port,
            handle: Some(handle),
        }
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        // A single probe connection unblocks the accept loop; the listener
        // and thread are then dropped without joining (the accept loop
        // exits once the listener is gone, and tests end with the process).
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.handle.take();
    }
}

pub struct Req {
    pub path: String,
}

impl Req {
    fn read(stream: &mut std::net::TcpStream) -> std::io::Result<Req> {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf)?;
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
        Ok(Req { path })
    }
}
