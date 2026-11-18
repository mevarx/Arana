//! Media channel integration tests: YouTube (yt-dlp read/search/subtitle),
//! the transcription pipeline (ffmpeg + Whisper multipart against a local
//! fixture) and the SSRF corpus ported from upstream `test_transcribe.py`.
//!
//! Tool-dependent paths run against fake `.cmd` yt-dlp shims (Windows batch
//! spawn works through the runner) and the real system ffmpeg when present;
//! live-network probes are `#[ignore]`d following `live_smoke.rs`:
//!
//!     cargo test -p reach-channels --test media -- --ignored --nocapture

use reach_channels::media;
use reach_tools::ssrf;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Env mutations are process-global: serialize every test that touches them.
/// An async mutex (unlike `std::sync::Mutex`) may be held across awaits and
/// has no poisoning, so a failing test cannot cascade into the others.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

/// Saves the previous values of env vars and restores them on drop.
struct EnvGuard(Vec<(String, Option<String>)>);

impl EnvGuard {
    fn set(vars: &[(&str, Option<&str>)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((key.to_string(), std::env::var(key).ok()));
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }
}

/// Write an executable fake tool for this platform from the small batch
/// dialect the fixtures use (`@echo off`, `echo TEXT`, `echo TEXT> file`,
/// `echo.>> file`, `type file`).
fn write_fake_tool(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{name}.cmd"));
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push_str("\r\n");
        }
        std::fs::write(&path, body).expect("write fake tool");
        return path;
    }
    // POSIX runners have no cmd.exe: translate the fixture into a real sh
    // script with the executable bit, or the spawn fails with EACCES.
    let path = dir.join(name);
    let mut body = String::from("#!/bin/sh\n");
    for line in lines {
        body.push_str(&shell_line(line));
        body.push('\n');
    }
    std::fs::write(&path, body).expect("write fake tool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tool");
    }
    path
}

/// Translate one fixture line into POSIX shell. Unsupported syntax is a panic
/// so a fixture can never silently do nothing on Unix.
#[cfg_attr(windows, allow(dead_code))]
fn shell_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("@echo off") {
        return ":".into();
    }
    if trimmed.eq_ignore_ascii_case("echo.") || trimmed.eq_ignore_ascii_case("echo") {
        return "printf '\\n'".into();
    }
    if let Some(code) = trimmed.strip_prefix("exit /b ") {
        return format!("exit {}", code.trim());
    }
    if trimmed.eq_ignore_ascii_case("exit /b") {
        return "exit 0".into();
    }
    if let Some(path) = trimmed.strip_prefix("type ") {
        return format!("cat {}", path.trim());
    }
    if !trimmed.to_ascii_lowercase().starts_with("echo") {
        panic!("unsupported fixture line for POSIX translation: {trimmed}");
    }
    if let Some(text) = trimmed
        .strip_prefix("echo ")
        .and_then(|line| line.strip_suffix(" 1>&2"))
    {
        let text = text.replace('\'', "'\\''");
        return format!("printf '%s\\n' '{text}' >&2");
    }
    let mut content = String::new();
    let mut target = String::new();
    let mut append = false;
    let mut redirecting = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '^' => {
                if let Some(next) = chars.next() {
                    if redirecting {
                        target.push(next);
                    } else {
                        content.push(next);
                    }
                }
            }
            '>' if !redirecting => {
                if chars.peek() == Some(&'>') {
                    chars.next();
                    append = true;
                }
                redirecting = true;
            }
            _ if redirecting => target.push(ch),
            _ => content.push(ch),
        }
    }
    let raw = content
        .strip_prefix("echo")
        .unwrap_or(&content)
        .trim_start();
    // `echo.` is batch for a blank line, with or without a redirect.
    let text = if raw == "." { "" } else { raw };
    let content = text.replace('\'', "'\\''");
    let target = target.trim();
    if target.is_empty() {
        format!("printf '%s\\n' '{content}'")
    } else {
        format!(
            "printf '%s\\n' '{content}' {} '{target}'",
            if append { ">>" } else { ">" }
        )
    }
}

#[test]
fn batch_fixtures_translate_to_shell() {
    // Golden translations: the POSIX fake tool must reproduce exactly what the
    // batch shim does, including the escaped subtitle arrow and blank lines.
    assert_eq!(shell_line("@echo off"), ":");
    assert_eq!(
        shell_line("echo WEBVTT> vid.zh.vtt"),
        "printf '%s\\n' 'WEBVTT' > 'vid.zh.vtt'"
    );
    assert_eq!(
        shell_line("echo 00:00:01.000 --^> 00:00:02.000>> vid.zh.vtt"),
        "printf '%s\\n' '00:00:01.000 --> 00:00:02.000' >> 'vid.zh.vtt'"
    );
    assert_eq!(
        shell_line("echo.>> vid.zh.vtt"),
        "printf '%s\\n' '' >> 'vid.zh.vtt'"
    );
    assert_eq!(
        shell_line("echo Me at the zoo"),
        "printf '%s\\n' 'Me at the zoo'"
    );
    assert_eq!(shell_line("type response.json"), "cat response.json");
    assert_eq!(shell_line("exit /b 1"), "exit 1");
    assert_eq!(
        shell_line("echo ERROR: unavailable 1>&2"),
        "printf '%s\\n' 'ERROR: unavailable' >&2"
    );
}

async fn run(
    channel: &str,
    action: &str,
    backend: &str,
    params: Value,
    state_dir: &Path,
) -> Result<Vec<Value>, reach_core::ReachError> {
    media::execute(
        channel,
        action,
        backend,
        &params,
        Duration::from_secs(30),
        state_dir,
    )
    .await
}

// ── youtube read / search against a fake yt-dlp ──

#[tokio::test]
async fn youtube_read_maps_ytdlp_json_to_item() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("ARANA_YTDLP_BIN", None)]);
    let dir = tempfile::tempdir().unwrap();
    let fake = write_fake_tool(
        dir.path(),
        "yt-dlp",
        &[
            "@echo off",
            "echo {\"id\": \"jNQXAC9IVRw\", \"title\": \"Me at the zoo\", \"uploader\": \"jawed\", \"channel\": \"jawed\", \"upload_date\": \"20050424\", \"webpage_url\": \"https://www.youtube.com/watch?v=jNQXAC9IVRw\", \"description\": \"The first video\", \"duration\": 19, \"view_count\": 1000, \"like_count\": 10, \"channel_id\": \"UC123\", \"tags\": [\"zoo\"], \"categories\": [\"Pets\"]}",
        ],
    );
    std::env::set_var("ARANA_YTDLP_BIN", &fake);
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "read",
        "yt-dlp",
        json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect("read succeeds");
    assert_eq!(items.len(), 1);
    let item = &items[0];
    // `read` prefers `uploader` for the author (upstream `_ytdlp_read`).
    assert_eq!(item["title"], "Me at the zoo");
    assert_eq!(item["author"], "jawed");
    assert_eq!(item["published_at"], "2005-04-24");
    assert_eq!(item["text"], "The first video");
    assert_eq!(item["url"], "https://www.youtube.com/watch?v=jNQXAC9IVRw");
    assert_eq!(item["raw"]["id"], "jNQXAC9IVRw");
    assert_eq!(item["raw"]["duration"], 19);
    assert_eq!(item["raw"]["view_count"], 1000);
    assert_eq!(item["raw"]["like_count"], 10);
    assert_eq!(item["raw"]["channel_id"], "UC123");
    assert_eq!(item["raw"]["tags"][0], "zoo");
    assert_eq!(item["raw"]["categories"][0], "Pets");
    assert_eq!(item.as_object().unwrap().len(), 6);
}

#[tokio::test]
async fn youtube_read_rejects_non_youtube_targets() {
    let state = tempfile::tempdir().unwrap();
    for target in ["https://vimeo.com/123", "not-a-target", ""] {
        let err = run(
            "youtube",
            "read",
            "yt-dlp",
            json!({"target": target}),
            state.path(),
        )
        .await
        .expect_err("must reject");
        assert_eq!(err.code, "BAD_INPUT", "for {target}");
    }
    let err = run("youtube", "read", "yt-dlp", json!({}), state.path())
        .await
        .expect_err("missing target");
    assert_eq!(err.code, "BAD_INPUT");
}

#[tokio::test]
async fn youtube_search_prefers_channel_author_and_builds_watch_urls() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("ARANA_YTDLP_BIN", None)]);
    let dir = tempfile::tempdir().unwrap();
    let fake = write_fake_tool(
        dir.path(),
        "yt-dlp",
        &[
            "@echo off",
            "echo {\"id\": \"abc12345678\", \"title\": \"First\", \"channel\": \"Chan One\", \"view_count\": 10}",
            "echo {\"id\": \"def45678901\", \"title\": \"Second\", \"uploader\": \"Up Two\"}",
        ],
    );
    std::env::set_var("ARANA_YTDLP_BIN", &fake);
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "search",
        "yt-dlp",
        json!({"query": "python tutorial", "limit": 2}),
        state.path(),
    )
    .await
    .expect("search succeeds");
    assert_eq!(items.len(), 2);
    // `search` prefers `channel` for the author (upstream `_ytdlp_search`).
    assert_eq!(items[0]["author"], "Chan One");
    assert_eq!(
        items[0]["url"],
        "https://www.youtube.com/watch?v=abc12345678"
    );
    assert_eq!(items[1]["author"], "Up Two");
    assert_eq!(
        items[1]["url"],
        "https://www.youtube.com/watch?v=def45678901"
    );
}

#[tokio::test]
async fn youtube_search_rejects_bad_limit() {
    let state = tempfile::tempdir().unwrap();
    let err = run(
        "youtube",
        "search",
        "yt-dlp",
        json!({"query": "x", "limit": 0}),
        state.path(),
    )
    .await
    .expect_err("zero limit");
    assert_eq!(err.code, "BAD_INPUT");
}

// ── yt-dlp failure taxonomy through the real runner ──

async fn failure_code(lines: &[&str]) -> reach_core::ReachError {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("ARANA_YTDLP_BIN", None)]);
    let dir = tempfile::tempdir().unwrap();
    let fake = write_fake_tool(dir.path(), "yt-dlp", lines);
    std::env::set_var("ARANA_YTDLP_BIN", &fake);
    let state = tempfile::tempdir().unwrap();
    run(
        "youtube",
        "read",
        "yt-dlp",
        json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect_err("fake always fails")
}

#[tokio::test]
async fn ytdlp_video_unavailable_maps_to_not_found() {
    let err = failure_code(&[
        "@echo off",
        "echo ERROR: [youtube] x: Video unavailable 1>&2",
        "exit /b 1",
    ])
    .await;
    assert_eq!(err.code, "NOT_FOUND");
    assert!(err.message.contains("yt-dlp exited 1"));
}

#[tokio::test]
async fn ytdlp_bot_check_maps_to_platform_blocked_with_custom_hint() {
    let err = failure_code(&[
        "@echo off",
        "echo ERROR: Please sign in to confirm you are not a bot 1>&2",
        "exit /b 1",
    ])
    .await;
    assert_eq!(err.code, "PLATFORM_BLOCKED");
    assert_eq!(
        err.hint,
        "YouTube returned a verification challenge. Stop this request; this does not establish that the public video requires login."
    );
}

#[tokio::test]
async fn ytdlp_rate_limit_maps_only_on_a_real_429() {
    let err = failure_code(&[
        "@echo off",
        "echo HTTP Error 429: Too Many Requests 1>&2",
        "exit /b 1",
    ])
    .await;
    assert_eq!(err.code, "RATE_LIMITED");
    // "4" and "29" appearing separately must NOT be read as 429.
    let err = failure_code(&[
        "@echo off",
        "echo gave up after 4 tries in 129 seconds 1>&2",
        "exit /b 1",
    ])
    .await;
    assert_eq!(err.code, "UPSTREAM_BROKEN", "must not misread 4/129 as 429");
}

// ── youtube subtitle against a fake yt-dlp that writes VTTs ──

#[tokio::test]
async fn youtube_subtitle_ranks_wanted_languages_and_collapses_repeats() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("ARANA_YTDLP_BIN", None)]);
    let dir = tempfile::tempdir().unwrap();
    // Writes two subtitle files into the work dir (the runner's cwd) and
    // prints the video title, like yt-dlp `--print` does.
    let fake = write_fake_tool(
        dir.path(),
        "yt-dlp",
        &[
            "@echo off",
            "echo WEBVTT> vid.zh.vtt",
            "echo 00:00:01.000 --^> 00:00:02.000>> vid.zh.vtt",
            "echo hello zh caption>> vid.zh.vtt",
            "echo.>> vid.zh.vtt",
            "echo 00:00:02.000 --^> 00:00:03.000>> vid.zh.vtt",
            "echo hello zh caption>> vid.zh.vtt",
            "echo WEBVTT> vid.en.vtt",
            "echo 00:00:01.000 --^> 00:00:02.000>> vid.en.vtt",
            "echo hello en caption>> vid.en.vtt",
            "echo Me at the zoo",
        ],
    );
    std::env::set_var("ARANA_YTDLP_BIN", &fake);
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "subtitle",
        "yt-dlp",
        json!({"target": "jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect("subtitle succeeds");
    assert_eq!(items.len(), 1);
    let item = &items[0];
    // The rolling duplicate collapses; the preferred language (zh) wins.
    assert_eq!(item["title"], "Me at the zoo");
    assert_eq!(item["text"], "hello zh caption");
    assert_eq!(item["url"], "https://www.youtube.com/watch?v=jNQXAC9IVRw");
    assert_eq!(item["raw"]["lang"], "zh");
    // The work directory is cleaned up afterwards.
    let tmp_root = state.path().join("tmp");
    let leftovers: Vec<_> = std::fs::read_dir(&tmp_root)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "work dirs removed, got {leftovers:?}");
}

#[tokio::test]
async fn youtube_subtitle_without_files_is_not_found() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("ARANA_YTDLP_BIN", None)]);
    let dir = tempfile::tempdir().unwrap();
    let fake = write_fake_tool(dir.path(), "yt-dlp", &["@echo off", "echo Me at the zoo"]);
    std::env::set_var("ARANA_YTDLP_BIN", &fake);
    let state = tempfile::tempdir().unwrap();
    let err = run(
        "youtube",
        "subtitle",
        "yt-dlp",
        json!({"target": "jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect_err("no vtt produced");
    assert_eq!(err.code, "NOT_FOUND");
    assert_eq!(
        err.message,
        "no subtitles in zh-Hans, zh, en, en-orig for this video"
    );
}

#[tokio::test]
async fn execute_rejects_unknown_action_backend_pairs() {
    let state = tempfile::tempdir().unwrap();
    let err = run(
        "youtube",
        "read",
        "whisper",
        json!({"target": "jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect_err("mismatched backend");
    assert_eq!(err.code, "BAD_INPUT");
    let err = run("vimeo", "read", "yt-dlp", json!({}), state.path())
        .await
        .expect_err("unknown channel");
    assert_eq!(err.code, "BAD_INPUT");
}

// ── provider configuration gates ──

#[tokio::test]
async fn transcribe_without_any_key_is_need_setup_with_free_key_hint() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("GROQ_API_KEY", None), ("OPENAI_API_KEY", None)]);
    let state = tempfile::tempdir().unwrap();
    let err = run(
        "youtube",
        "transcribe",
        "whisper",
        json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect_err("no key");
    assert_eq!(err.code, "NEED_SETUP");
    assert_eq!(err.message, "no Whisper API key configured");
    assert!(err.hint.contains("console.groq.com"), "hint: {}", err.hint);
    assert!(
        err.hint.contains("configure groq-key"),
        "hint: {}",
        err.hint
    );
}

#[tokio::test]
async fn xiaoyuzhou_precheck_rejections() {
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("GROQ_API_KEY", None), ("OPENAI_API_KEY", None)]);
    let state = tempfile::tempdir().unwrap();
    // Not a xiaoyuzhoufm.com link.
    let err = run(
        "xiaoyuzhou",
        "transcribe",
        "groq-whisper",
        json!({"target": "https://example.com/episode/1"}),
        state.path(),
    )
    .await
    .expect_err("wrong host");
    assert_eq!(err.code, "BAD_INPUT");
    assert_eq!(
        err.message,
        "target must be a xiaoyuzhoufm.com episode link"
    );
    // Right host, no Groq key.
    let err = run(
        "xiaoyuzhou",
        "transcribe",
        "groq-whisper",
        json!({"target": "https://www.xiaoyuzhoufm.com/episode/abc"}),
        state.path(),
    )
    .await
    .expect_err("no key");
    assert_eq!(err.code, "NEED_SETUP");
    assert_eq!(err.message, "no Groq API key configured");
    assert!(err.hint.contains("console.groq.com"), "hint: {}", err.hint);
}

// ── SSRF corpus (port of upstream TestDownloadAudioSafety) ──
//
// Every rejected target must fail before any tool resolves or spawns; a
// provider key is configured so the pipeline reaches the download stage.

#[tokio::test]
async fn transcribe_rejects_private_targets_before_any_download() {
    // This test sets GROQ_API_KEY for its whole body; without the lock it
    // races every other test that expects the variable to be absent.
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[("GROQ_API_KEY", Some("test-key")), ("OPENAI_API_KEY", None)]);
    let state = tempfile::tempdir().unwrap();
    // The rejected corpus from upstream tests/test_transcribe.py.
    let rejected: Vec<String> = [
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
        // Full-width digit spellings of 127.0.0.1 / 2130706433 / 0x7f000001.
        "http://１２７.０.０.１/a.mp3",
        "http://２１３０７０６４３３/a.mp3",
        "http://０x７f０００００１/a.mp3",
        // Homoglyph hostnames for localhost.
        "http://ⓛⓞⓒⓐⓛⓗⓞⓢⓣ/a.mp3",
        "http://ℓocalhost/a.mp3",
        // Percent-encoded hosts.
        "http://%31%32%37.0.0.1/a.mp3",
        "http://127%2e0%2e0%2e1/a.mp3",
        "http://local%68ost/a.mp3",
        "http://localhost/a.mp3",
        "http://metadata.google.internal/a.mp3",
        "http://sub.localhost/a.mp3",
    ]
    .iter()
    .map(|s| s.to_string())
    // Userinfo-disguised loopback with a literal backslash.
    .chain(std::iter::once(format!(
        "http://{}\\@example.com/a.mp3",
        "127.0.0.1"
    )))
    .collect();
    for url in &rejected {
        let deadline = Instant::now() + Duration::from_secs(10);
        let err = media::transcribe_source(
            url,
            state.path(),
            deadline,
            media::TranscribeOptions::default(),
        )
        .await
        .expect_err("must reject");
        assert_eq!(err.code, "BAD_INPUT", "for {url}");
        assert!(err.message.contains("SSRF"), "for {url}: {err}");
    }
}

#[test]
fn ssrf_allows_public_literal_addresses_and_bare_hosts() {
    for url in [
        "https://1.1.1.1/a.mp3",
        "https://8.8.8.8/a.mp3",
        // Octal dotted quad that denotes a public address, not loopback.
        "http://010.010.010.010/a.mp3",
        "youtu.be/abc123",
        "https://youtu.be/abc123",
    ] {
        assert!(
            ssrf::assert_safe_public_url(url).is_ok(),
            "must allow {url}"
        );
    }
}

// ── Whisper pipeline against a local multipart fixture ──

struct ProviderFixture {
    base: String,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
}

/// True when `data` contains a complete HTTP/1.1 chunked body.
fn chunked_body_complete(mut data: &[u8]) -> bool {
    loop {
        let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") else {
            return false;
        };
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            return false;
        };
        data = &data[line_end + 2..];
        if size == 0 {
            return data.starts_with(b"\r\n");
        }
        if data.len() < size + 2 {
            return false;
        }
        data = &data[size + 2..];
    }
}

/// Serve queued `(status, body)` responses on a loopback port, recording the
/// raw bytes of every request (headers + multipart body) for assertions.
async fn spawn_provider(responses: Vec<(u16, String)>) -> ProviderFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let base = format!("http://{}", listener.local_addr().expect("fixture addr"));
    let requests: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let queue: Arc<Mutex<Vec<(u16, String)>>> = Arc::new(Mutex::new(responses));
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let recorded = Arc::clone(&recorded);
            let queue = Arc::clone(&queue);
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 16384];
                // Read headers, then the body (Content-Length or chunked).
                loop {
                    let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
                    let done = head_end.map(|end| {
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_uppercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("CONTENT-LENGTH:"))
                            .and_then(|v| v.trim().parse::<usize>().ok());
                        if let Some(length) = length {
                            buf.len() >= end + length
                        } else if head.contains("TRANSFER-ENCODING: CHUNKED") {
                            chunked_body_complete(&buf[end..])
                        } else {
                            true
                        }
                    });
                    if done == Some(true) {
                        break;
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                recorded.lock().unwrap().push(buf.clone());
                // Take the next queued response in one short critical
                // section: holding the guard through `unwrap_or_else` would
                // deadlock on the non-reentrant std Mutex.
                let (status, body) = {
                    let mut queue = queue.lock().unwrap();
                    if queue.is_empty() {
                        (500u16, "queue empty".to_string())
                    } else {
                        queue.remove(0)
                    }
                };
                let reason = match status {
                    200 => "OK",
                    429 => "Too Many Requests",
                    _ => "Internal Server Error",
                };
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    ProviderFixture { base, requests }
}

/// The system ffmpeg, if installed (pipeline tests need a real encoder).
fn system_ffmpeg() -> Option<PathBuf> {
    which::which("ffmpeg").ok()
}

#[tokio::test]
async fn whisper_pipeline_posts_multipart_to_groq() {
    let Some(ffmpeg) = system_ffmpeg() else {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    };
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[
        ("GROQ_API_KEY", Some("test-groq-key")),
        ("OPENAI_API_KEY", None),
    ]);
    let fixture = spawn_provider(vec![(200, "fixture transcript text".to_string())]).await;
    let _endpoints = EnvGuard::set(&[
        ("ARANA_GROQ_TRANSCRIBE_ENDPOINT", Some(&fixture.base)),
        ("ARANA_OPENAI_TRANSCRIBE_ENDPOINT", None),
    ]);

    // A real 2-second tone, so duration probe / compress / single chunk run.
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    let out = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-ac",
            "1",
            "-ar",
            "16000",
        ])
        .arg(&wav)
        .output()
        .expect("generate sine");
    assert!(
        out.status.success(),
        "ffmpeg sine: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let state = tempfile::tempdir().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome = media::transcribe_source(
        wav.to_string_lossy().as_ref(),
        state.path(),
        deadline,
        media::TranscribeOptions::default(),
    )
    .await
    .expect("pipeline succeeds");

    assert_eq!(outcome.text, "fixture transcript text");
    assert!(
        (1.0..=4.0).contains(&outcome.duration_seconds),
        "duration ~2s, got {}",
        outcome.duration_seconds
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one provider call");
    let request = &requests[0];
    let head = String::from_utf8_lossy(&request[..request.len().min(4096)]).to_string();
    assert!(head.contains("POST / HTTP/1.1"), "request line: {head}");
    // Dummy fixture key, not a credential: the provider must receive the real
    // bearer token or it could never authenticate.
    assert!(
        head.contains("authorization: Bearer test-groq-key"),
        "{head}"
    );
    assert!(head.contains("multipart/form-data; boundary="), "{head}");
    // The upstream multipart shape: model, response_format=text, file part.
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: audio/m4a"),
        "head: {head}"
    );
    for needle in [
        &b"model\"\r\n\r\nwhisper-large-v3"[..],
        b"response_format\"\r\n\r\ntext",
        b"filename=\"compressed.m4a\"",
    ] {
        assert!(
            request.windows(needle.len()).any(|w| w == needle),
            "multipart must contain {:?}",
            String::from_utf8_lossy(needle)
        );
    }
}

#[tokio::test]
async fn whisper_429_does_not_retry_for_youtube() {
    let Some(ffmpeg) = system_ffmpeg() else {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    };
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[
        ("GROQ_API_KEY", Some("test-groq-key")),
        ("OPENAI_API_KEY", None),
    ]);
    let fixture = spawn_provider(vec![(429, "rate limited; retry in 9m".to_string())]).await;
    let _endpoints = EnvGuard::set(&[("ARANA_GROQ_TRANSCRIBE_ENDPOINT", Some(&fixture.base))]);

    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    let out = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-ac",
            "1",
            "-ar",
            "16000",
        ])
        .arg(&wav)
        .output()
        .expect("generate sine");
    assert!(out.status.success());

    let state = tempfile::tempdir().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    // youtube path: default opts — no 429 retry.
    let err = media::transcribe_source(
        wav.to_string_lossy().as_ref(),
        state.path(),
        deadline,
        media::TranscribeOptions::default(),
    )
    .await
    .expect_err("429 fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("HTTP 429"), "message: {err}");
    assert_eq!(fixture.requests.lock().unwrap().len(), 1, "no retry");
}

#[tokio::test]
async fn xiaoyuzhou_opts_wait_and_retry_once_on_429() {
    let Some(ffmpeg) = system_ffmpeg() else {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    };
    let _guard = env_lock().await;
    let _env = EnvGuard::set(&[
        ("GROQ_API_KEY", Some("test-groq-key")),
        ("OPENAI_API_KEY", None),
    ]);
    let fixture = spawn_provider(vec![
        (429, "rate limited; retry in 0m".to_string()),
        (200, "recovered after wait".to_string()),
    ])
    .await;
    let _endpoints = EnvGuard::set(&[("ARANA_GROQ_TRANSCRIBE_ENDPOINT", Some(&fixture.base))]);

    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    let out = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-ac",
            "1",
            "-ar",
            "16000",
        ])
        .arg(&wav)
        .output()
        .expect("generate sine");
    assert!(out.status.success());

    let state = tempfile::tempdir().unwrap();
    // "in 0m" wants 30s; the deadline clamps the wait so the test stays fast.
    let deadline = Instant::now() + Duration::from_secs(5);
    let opts = media::TranscribeOptions {
        language: Some("zh"),
        prompt: Some("以下是中文播客"),
        retry_rate_limited: true,
    };
    let outcome =
        media::transcribe_source(wav.to_string_lossy().as_ref(), state.path(), deadline, opts)
            .await
            .expect("retry succeeds");
    assert_eq!(outcome.text, "recovered after wait");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "exactly one retry");
    // The xiaoyuzhou form fields ride along in the multipart body.
    let request = &requests[0];
    let language_needle = b"language\"\r\n\r\nzh".to_vec();
    let mut prompt_needle = b"prompt\"\r\n\r\n".to_vec();
    prompt_needle.extend_from_slice("以下是中文播客".as_bytes());
    for needle in [&language_needle, &prompt_needle] {
        assert!(
            request
                .windows(needle.len())
                .any(|w| w == needle.as_slice()),
            "multipart must contain {:?}",
            String::from_utf8_lossy(needle)
        );
    }
}

#[tokio::test]
async fn chunk_audio_splits_real_audio_into_segments() {
    let Some(ffmpeg) = system_ffmpeg() else {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("chunks");
    std::fs::create_dir_all(&out_dir).unwrap();
    // 20 minutes + 10 seconds of real, decodable audio. At the production
    // 10-minute segment size this must yield exactly three chunks. Smaller
    // segments are rejected by the MAX_CHUNKS budget guard before ffmpeg runs.
    let wav = dir.path().join("silence.wav");
    let out = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=16000:cl=mono",
            "-t",
            "1210",
        ])
        .arg(&wav)
        .output()
        .expect("generate silence");
    assert!(
        out.status.success(),
        "ffmpeg silence: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let state = tempfile::tempdir().unwrap();
    let chunks = media::chunk_audio_with_segment(
        &wav,
        &out_dir,
        state.path(),
        600,
        Instant::now() + Duration::from_secs(120),
    )
    .await
    .expect("chunks");
    assert_eq!(
        chunks.len(),
        3,
        "two 10-minute segments plus a 10s tail, got {chunks:?}"
    );
    for chunk in &chunks {
        assert!(chunk.is_file());
    }
}

// ── live probes (real network + the pinned system yt-dlp) ──
//
//     cargo test -p reach-channels --test media -- --ignored --nocapture

#[tokio::test]
#[ignore = "live network: youtube + yt-dlp"]
async fn live_youtube_read_fixed_video() {
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "read",
        "yt-dlp",
        json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect("live read");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Me at the zoo");
    assert_eq!(items[0]["published_at"], "2005-04-24");
    assert!(items[0]["text"].as_str().unwrap().len() > 20);
}

#[tokio::test]
#[ignore = "live network: youtube + yt-dlp"]
async fn live_youtube_subtitle_fixed_video() {
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "subtitle",
        "yt-dlp",
        json!({"target": "jNQXAC9IVRw"}),
        state.path(),
    )
    .await
    .expect("live subtitle");
    assert_eq!(items.len(), 1);
    let text = items[0]["text"].as_str().expect("text");
    assert!(!text.is_empty(), "auto captions exist for this video");
    assert!(items[0]["raw"]["lang"].is_string());
}

#[tokio::test]
#[ignore = "live network: youtube + yt-dlp"]
async fn live_youtube_search() {
    let state = tempfile::tempdir().unwrap();
    let items = run(
        "youtube",
        "search",
        "yt-dlp",
        json!({"query": "yt-dlp github repository", "limit": 2}),
        state.path(),
    )
    .await
    .expect("live search");
    assert!(!items.is_empty(), "expected search results");
    for item in &items {
        assert!(item["url"].as_str().unwrap().contains("watch?v="));
    }
}
