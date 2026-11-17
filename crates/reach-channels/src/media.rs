//! Media channels: YouTube (yt-dlp read/search/subtitle + Whisper
//! transcription) and Xiaoyuzhou podcasts (page → audio → Groq Whisper).
//!
//! Ports `channels/youtube.py`, `channels/xiaoyuzhou.py` and the
//! `transcribe.py` pipeline (incl. the constants and error taxonomy of the
//! upstream `scripts/transcribe_xiaoyuzhou.sh`): yt-dlp download → ffmpeg
//! duration/size budget checks → compress/chunk → Groq (`whisper-large-v3`)
//! or OpenAI (`whisper-1`). Every child runs through the robust runner
//! (hidden, deadline, tree-kill); user URLs pass the SSRF guard before any
//! tool sees them, and one overall deadline spans the whole
//! extraction/transcription pipeline.

use reach_core::{make_item, ReachError, Result};
use reach_tools::registry;
use reach_tools::runner::{self, CommandSpec};
use reach_tools::{config, ssrf};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Whisper API limit is 25MB; leave headroom for multipart overhead.
const SIZE_LIMIT_BYTES: u64 = 24 * 1024 * 1024;
/// 10 min — small enough that boundary cuts rarely lose meaning.
const CHUNK_SECONDS: u64 = 600;
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
/// 4 hours at the standard 10-minute segment size.
const MAX_CHUNKS: usize = 24;
const MAX_TOTAL_CHUNK_BYTES: u64 = 96 * 1024 * 1024;
const MAX_AUDIO_SECONDS: u64 = MAX_CHUNKS as u64 * CHUNK_SECONDS;
const PROBE_TIMEOUT_SECONDS: u64 = 30;
const CHUNK_UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Download cap for the podcast episode page (upstream script: 5 MiB).
const MAX_PAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Page-fetch cap, mirroring the script's `PAGE_TIMEOUT`.
const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

const GROQ_TRANSCRIBE_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const OPENAI_TRANSCRIBE_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";

/// Sentinel: no provider key is configured (mapped to `NEED_SETUP` upstream).
const NO_KEY_MESSAGE: &str = "no Whisper API key configured";

/// One transcription provider.
struct Provider {
    name: &'static str,
    endpoint: String,
    model: &'static str,
    key: String,
}

/// Successful transcription outcome: joined text plus source duration.
#[derive(Debug)]
pub struct Transcription {
    pub text: String,
    /// Duration of the source audio in seconds (used by the xiaoyuzhou header).
    pub duration_seconds: f64,
}

/// Entry point used by dispatch: run one media-channel action.
///
/// `backend` is the upstream backend label declared for the action in the
/// registry (`yt-dlp` for youtube read/search/subtitle, `whisper` for the
/// youtube transcription, `groq-whisper` for xiaoyuzhou).
pub async fn execute(
    channel: &str,
    action: &str,
    backend: &str,
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    match (channel, action, backend) {
        ("youtube", "read", "yt-dlp") => youtube_read(params, timeout, state_dir).await,
        ("youtube", "search", "yt-dlp") => youtube_search(params, timeout, state_dir).await,
        ("youtube", "subtitle", "yt-dlp") => youtube_subtitle(params, timeout, state_dir).await,
        ("youtube", "transcribe", "whisper") => {
            youtube_transcribe(params, timeout, state_dir).await
        }
        ("xiaoyuzhou", "transcribe", "groq-whisper") => {
            xiaoyuzhou_transcribe(params, timeout, state_dir).await
        }
        _ => Err(ReachError::new(
            "BAD_INPUT",
            format!("unknown media channel/action/backend: {channel} {action} {backend} (valid: youtube read|search|subtitle via yt-dlp, youtube transcribe via whisper, xiaoyuzhou transcribe via groq-whisper)"),
        )),
    }
}

// ── shared helpers ──

fn require_str<'a>(params: &'a Value, name: &str) -> Result<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ReachError::new("BAD_INPUT", format!("missing required parameter: {name}")))
}

fn bad_input(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Remaining budget before the overall deadline (never zero, never panics).
fn remaining(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

fn first_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// ── YouTube ──

static VIDEO_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());

/// Accepts an 11-character video id or any youtube.com/youtu.be link
/// (upstream `_video_url`).
pub fn video_url(target: &str) -> Result<String> {
    let t = target.trim();
    if VIDEO_ID_RE.is_match(t) {
        return Ok(format!("https://www.youtube.com/watch?v={t}"));
    }
    let candidate = if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    if ssrf::host_matches(&candidate, &["youtube.com", "youtu.be"]) {
        Ok(candidate)
    } else {
        Err(bad_input(
            "target must be a YouTube link or 11-character video id",
        ))
    }
}

/// yt-dlp trusts its bundled certifi and ignores SSL_CERT_FILE; behind a
/// TLS-inspecting proxy (corporate, school) every request then fails
/// CERTIFICATE_VERIFY_FAILED (upstream `ytdlp_tls_args`).
fn ytdlp_tls_args() -> Vec<String> {
    if std::env::var_os("SSL_CERT_FILE").is_some() || std::env::var_os("SSL_CERT_DIR").is_some() {
        return vec!["--compat-options".into(), "no-certifi".into()];
    }
    Vec::new()
}

fn has_runtime(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(format!("{name}.exe")).exists() || dir.join(name).exists())
        })
        .unwrap_or(false)
}

/// The fixed prefix every yt-dlp invocation from the channel gets
/// (upstream `_ytdlp_argv`: `--no-warnings`, TLS trust-store args and, when
/// only Node is available, `--js-runtimes node`).
fn ytdlp_base_args() -> Vec<String> {
    let mut argv = vec!["--no-warnings".to_string()];
    argv.extend(ytdlp_tls_args());
    if !has_runtime("deno") && has_runtime("node") {
        argv.push("--js-runtimes".into());
        argv.push("node".into());
    }
    argv
}

/// Run the yt-dlp installed with arana (private copy first, then PATH),
/// mapping known failure text onto the unified error codes
/// (upstream `_run_ytdlp`).
async fn run_ytdlp(
    args: &[String],
    timeout: Duration,
    cwd: Option<&Path>,
    state_dir: &Path,
) -> Result<String> {
    let exe = registry::resolve("yt-dlp", state_dir)
        .ok_or_else(|| ReachError::new("NEED_SETUP", "yt-dlp is not installed"))?;
    let mut argv = ytdlp_base_args();
    argv.extend(args.iter().cloned());
    let mut spec = CommandSpec::new(&exe, "yt-dlp");
    spec.args = argv;
    spec.timeout = timeout;
    if let Some(dir) = cwd {
        spec.cwd = Some(dir.to_path_buf());
    }
    let out = runner::run_captured(spec).await?;
    if !out.success() {
        return Err(map_ytdlp_failure(out.code(), &out.stderr));
    }
    Ok(out.stdout)
}

/// Upstream `_run_ytdlp` error mapping, from the child's stderr. The base
/// message reproduces upstream `run_cmd`'s `CommandFailed` shape
/// (`{name} exited {code}: {last stderr line or 'no error output'}`).
fn map_ytdlp_failure(code: i32, stderr: &str) -> ReachError {
    let err = stderr.to_lowercase();
    let last = runner::last_line(stderr).trim();
    let message = if last.is_empty() {
        format!("yt-dlp exited {code}: no error output")
    } else {
        format!("yt-dlp exited {code}: {last}")
    };
    if [
        "video unavailable",
        "private video",
        "has been removed",
        "does not exist",
    ]
    .iter()
    .any(|s| err.contains(s))
    {
        return ReachError::new("NOT_FOUND", message);
    }
    if err.contains("sign in to confirm") || err.contains("not a bot") {
        let mut error = ReachError::new("PLATFORM_BLOCKED", message);
        error.hint = "YouTube returned a verification challenge. Stop this request; this does not establish that the public video requires login.".into();
        return error;
    }
    if err.contains("429") || err.contains("too many requests") {
        return ReachError::new("RATE_LIMITED", message);
    }
    ReachError::new("UPSTREAM_BROKEN", message)
}

/// `20260117` → `2026-01-17` (upstream `_iso_date`).
fn iso_date(yyyymmdd: &str) -> Option<String> {
    if yyyymmdd.len() == 8 && yyyymmdd.chars().all(|c| c.is_ascii_digit()) {
        Some(format!(
            "{}-{}-{}",
            &yyyymmdd[..4],
            &yyyymmdd[4..6],
            &yyyymmdd[6..8]
        ))
    } else {
        None
    }
}

/// The raw item fields kept from yt-dlp output (upstream `_RAW_KEYS`).
fn raw_info_fields(info: &Value) -> Value {
    json!({
        "id": info.get("id").cloned().unwrap_or(Value::Null),
        "duration": info.get("duration").cloned().unwrap_or(Value::Null),
        "view_count": info.get("view_count").cloned().unwrap_or(Value::Null),
        "like_count": info.get("like_count").cloned().unwrap_or(Value::Null),
        "channel_id": info.get("channel_id").cloned().unwrap_or(Value::Null),
        "tags": info.get("tags").cloned().unwrap_or(Value::Null),
        "categories": info.get("categories").cloned().unwrap_or(Value::Null),
    })
}

/// One normalized item from yt-dlp JSON. `author_uploader_first` selects the
/// upstream field preference: `uploader or channel` for `read`, `channel or
/// uploader` for `search`.
fn info_item(info: &Value, author_uploader_first: bool, url_fallback: Option<&str>) -> Value {
    let id = info.get("id").and_then(Value::as_str);
    let (first, second) = if author_uploader_first {
        ("uploader", "channel")
    } else {
        ("channel", "uploader")
    };
    let author = info
        .get(first)
        .and_then(Value::as_str)
        .or_else(|| info.get(second).and_then(Value::as_str))
        .map(str::to_string);
    let url = info
        .get("webpage_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| id.map(|vid| format!("https://www.youtube.com/watch?v={vid}")))
        .or_else(|| url_fallback.map(str::to_string));
    make_item(
        info.get("title")
            .and_then(Value::as_str)
            .map(str::to_string),
        url,
        author,
        info.get("upload_date")
            .and_then(Value::as_str)
            .and_then(iso_date),
        info.get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        raw_info_fields(info),
    )
}

async fn youtube_read(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let target = require_str(params, "target")?;
    let url = video_url(target)?;
    let args = vec!["--dump-json".into(), "--skip-download".into(), url];
    let out = run_ytdlp(&args, timeout, None, state_dir).await?;
    let last = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| ReachError::new("UPSTREAM_BROKEN", "yt-dlp returned no JSON output"))?;
    let info: Value = serde_json::from_str(last.trim())
        .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("yt-dlp JSON parse error: {e}")))?;
    Ok(vec![info_item(&info, true, None)])
}

async fn youtube_search(params: &Value, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let query = require_str(params, "query")?;
    let limit = match params.get("limit") {
        None | Some(Value::Null) => 5usize,
        Some(v) => {
            v.as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| bad_input("limit must be a positive integer"))? as usize
        }
    };
    let args = vec![
        "--flat-playlist".into(),
        "--dump-json".into(),
        format!("ytsearch{limit}:{query}"),
    ];
    let out = run_ytdlp(&args, timeout, None, state_dir).await?;
    let mut items = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let info: Value = serde_json::from_str(line.trim()).map_err(|e| {
            ReachError::new("UPSTREAM_BROKEN", format!("yt-dlp JSON parse error: {e}"))
        })?;
        items.push(info_item(&info, false, None));
    }
    Ok(items)
}

/// Pick the wanted-language VTT: preference order, then any
/// (upstream `rank`/`best` selection).
fn pick_best_vtt(files: &[String], wanted: &[String]) -> Option<(String, Option<String>)> {
    let best = files
        .iter()
        .min_by_key(|name| {
            let code = vtt_lang_code(name).unwrap_or_default();
            wanted
                .iter()
                .position(|w| w == code)
                .unwrap_or(wanted.len())
        })
        .cloned()?;
    let code = vtt_lang_code(&best).map(str::to_string);
    Some((best, code))
}

/// The language code in `<id>.<lang>.vtt` (upstream `name.rsplit(".", 2)[-2]`).
fn vtt_lang_code(name: &str) -> Option<&str> {
    if name.matches('.').count() < 2 {
        return None;
    }
    name.rsplit('.').nth(1)
}

async fn youtube_subtitle(
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    let target = require_str(params, "target")?;
    let lang = params
        .get("lang")
        .and_then(Value::as_str)
        .unwrap_or("zh-Hans,zh,en,en-orig");
    let url = video_url(target)?;
    let wanted: Vec<String> = lang
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let tmp = private_work_dir(state_dir)?;
    let args = vec![
        "--skip-download".into(),
        "--no-simulate".into(),
        "--write-subs".into(),
        "--write-auto-subs".into(),
        "--sub-langs".into(),
        wanted.join(","),
        "--sub-format".into(),
        "vtt".into(),
        "--print".into(),
        "%(title)s".into(),
        "-o".into(),
        "%(id)s.%(ext)s".into(),
        url.clone(),
    ];
    let out = run_ytdlp(&args, timeout, Some(tmp.path()), state_dir).await?;
    let mut files: Vec<String> = std::fs::read_dir(tmp.path())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".vtt"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let Some((best, code)) = pick_best_vtt(&files, &wanted) else {
        return Err(ReachError::new(
            "NOT_FOUND",
            format!("no subtitles in {} for this video", wanted.join(", ")),
        ));
    };
    let vtt = std::fs::read_to_string(tmp.path().join(&best))
        .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("subtitle read failed: {e}")))?;
    let text = vtt_to_text(&vtt);
    let title = out
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string());
    Ok(vec![make_item(
        title,
        Some(url),
        None,
        None,
        Some(text),
        json!({"lang": code}),
    )])
}

async fn youtube_transcribe(
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    let target = require_str(params, "target")?;
    let url = video_url(target)?;
    let deadline = Instant::now() + timeout;
    let outcome =
        match transcribe_source(&url, state_dir, deadline, TranscribeOptions::default()).await {
            Ok(outcome) => outcome,
            // Upstream `_whisper_transcribe`: NoProviderConfigured → NEED_SETUP
            // with the free-key hint, every other TranscribeError →
            // UPSTREAM_BROKEN carrying the message.
            Err(e) => return Err(map_transcribe_error(e)),
        };
    Ok(vec![make_item(
        None,
        Some(url),
        None,
        None,
        Some(outcome.text),
        Value::Null,
    )])
}

/// Upstream `_whisper_transcribe`'s exception mapping.
fn map_transcribe_error(e: ReachError) -> ReachError {
    if e.code == "NEED_SETUP" {
        if e.message == NO_KEY_MESSAGE {
            let mut error = ReachError::new("NEED_SETUP", "no Whisper API key configured");
            error.hint = "Ask the user for a free key from https://console.groq.com, then run `arana configure groq-key`.".into();
            return error;
        }
        return e;
    }
    ReachError::new("UPSTREAM_BROKEN", first_chars(&e.message, 300))
}

// ── WebVTT → text (port of upstream `vtt_to_text`) ──

static VTT_TIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\d{2,}:)?\d{2}:\d{2}\.\d{3}[ \t]+-->[ \t]+(?:\d{2,}:)?\d{2}:\d{2}\.\d{3}(?:[ \t]|$)",
    )
    .unwrap()
});
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Plain text from WebVTT: drop headers, timings, tags; collapse auto-caption
/// rolling repeats (port of upstream `vtt_to_text`).
pub fn vtt_to_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_cue = false;
    let mut block_start = true;
    let mut metadata = false;
    for line in vtt.lines() {
        let s = line.trim();
        if s.is_empty() {
            in_cue = false;
            block_start = true;
            metadata = false;
            continue;
        }
        if block_start {
            block_start = false;
            metadata = s == "NOTE"
                || s == "STYLE"
                || s == "REGION"
                || s.starts_with("NOTE ")
                || s.starts_with("NOTE\t");
        }
        if metadata {
            continue;
        }
        if VTT_TIME_RE.is_match(s) {
            in_cue = true;
            continue;
        }
        if !in_cue {
            // Headers and optional cue identifiers precede the timing line.
            continue;
        }
        let s = html_unescape(&TAG_RE.replace_all(s, "")).trim().to_string();
        if !s.is_empty() && out.last().map(|l| l != &s).unwrap_or(true) {
            out.push(s);
        }
    }
    out.join("\n")
}

/// Minimal HTML entity unescape (named + numeric), enough for captions.
fn html_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(end) = rest.find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "rsquo" => Some('\''),
            "nbsp" => Some(' '),
            _ => {
                if let Some(num) = entity.strip_prefix('#') {
                    let code = if let Some(hex) =
                        num.strip_prefix('x').or_else(|| num.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ── transcription pipeline (port of `transcribe.py`) ──

/// Provider/language options for one transcription run.
#[derive(Default)]
pub struct TranscribeOptions {
    /// ISO language hint sent to Whisper (xiaoyuzhou: `"zh"`).
    pub language: Option<&'static str>,
    /// Punctuation prompt (xiaoyuzhou's Chinese-punctuation prompt).
    pub prompt: Option<&'static str>,
    /// Wait and retry once on provider HTTP 429 (the xiaoyuzhou script does;
    /// `transcribe.py` does not). The wait is bounded by the deadline.
    pub retry_rate_limited: bool,
}

/// Configured providers in upstream auto order: groq, then openai. Provider
/// endpoints can be overridden for tests/operators via
/// `ARANA_GROQ_TRANSCRIBE_ENDPOINT` / `ARANA_OPENAI_TRANSCRIBE_ENDPOINT`.
fn configured_providers(state_dir: &Path) -> Vec<Provider> {
    let mut out = Vec::new();
    if let Some(key) = config::get(state_dir, "groq_api_key") {
        let endpoint = env_nonempty("ARANA_GROQ_TRANSCRIBE_ENDPOINT")
            .unwrap_or_else(|| GROQ_TRANSCRIBE_ENDPOINT.to_string());
        out.push(Provider {
            name: "groq",
            endpoint,
            model: "whisper-large-v3",
            key,
        });
    }
    if let Some(key) = config::get(state_dir, "openai_api_key") {
        let endpoint = env_nonempty("ARANA_OPENAI_TRANSCRIBE_ENDPOINT")
            .unwrap_or_else(|| OPENAI_TRANSCRIBE_ENDPOINT.to_string());
        out.push(Provider {
            name: "openai",
            endpoint,
            model: "whisper-1",
            key,
        });
    }
    out
}

/// A private work directory under `state_dir/tmp`, removed on drop
/// (upstream `private_temporary_directory`).
struct WorkDir(PathBuf);

impl WorkDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn private_work_dir(state_dir: &Path) -> Result<WorkDir> {
    let root = state_dir.join("tmp");
    reach_core::state::private_dir(&root)?;
    // Atomic create with a unique name: two concurrent transcriptions in the
    // same process must never share (and overwrite) one work directory.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    for _ in 0..64 {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = root.join(format!("arana-{pid}-{nanos:x}-{seq:x}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(WorkDir(dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ReachError::new(
        "UPSTREAM_BROKEN",
        "could not create a private work directory",
    ))
}

/// Resolve ffmpeg for the transcription pipeline. Upstream raises
/// `MissingDependency` ("ffmpeg not found (run `agent-reach add youtube`)"),
/// which the channel maps to UPSTREAM_BROKEN.
fn pipeline_ffmpeg(state_dir: &Path) -> Result<PathBuf> {
    registry::resolve("ffmpeg", state_dir).ok_or_else(|| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "ffmpeg not found (run `arana add youtube`)",
        )
    })
}

/// Download audio with yt-dlp into `out_dir`; return the resulting file
/// (upstream `download_audio`).
async fn download_audio(
    url: &str,
    out_dir: &Path,
    state_dir: &Path,
    deadline: Instant,
) -> Result<PathBuf> {
    ssrf::assert_safe_public_url(url)?;
    let ytdlp = registry::resolve("yt-dlp", state_dir)
        .ok_or_else(|| ReachError::new("NEED_SETUP", "yt-dlp is not installed"))?;
    // Upstream `_bin("ffmpeg")`: our private copy first, else the bare name.
    let ffmpeg = registry::resolve("ffmpeg", state_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ffmpeg".into());
    let template = out_dir.join("source.%(ext)s");
    let mut argv = ytdlp_tls_args();
    argv.extend(
        [
            "-x",
            "--audio-format",
            "m4a",
            "--audio-quality",
            "0",
            "--no-playlist",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    argv.push("--ffmpeg-location".into());
    argv.push(ffmpeg);
    argv.push("--max-filesize".into());
    argv.push(MAX_SOURCE_BYTES.to_string());
    argv.push("-o".into());
    argv.push(template.to_string_lossy().into_owned());
    argv.push("--".into());
    argv.push(url.to_string());
    let mut spec = CommandSpec::new(&ytdlp, "yt-dlp");
    spec.args = argv;
    spec.timeout = remaining(deadline).min(Duration::from_secs(1800));
    let out = runner::run_captured(spec).await?;
    if !out.success() {
        // Upstream `_run`: "{cmd[0]} failed (exit {code}): {stderr[:300]}";
        // the channel surfaces it as UPSTREAM_BROKEN.
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "yt-dlp failed (exit {}): {}",
                out.code(),
                first_chars(out.stderr.trim(), 300)
            ),
        ));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with("source."))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let audio = files.into_iter().next().ok_or_else(|| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "yt-dlp produced no output file (source may exceed {} MiB limit)",
                MAX_SOURCE_BYTES / (1024 * 1024)
            ),
        )
    })?;
    require_size_at_most(&audio, MAX_SOURCE_BYTES, "downloaded source")?;
    Ok(audio)
}

/// Return file size or fail before expensive downstream processing
/// (upstream `_require_size_at_most`).
fn require_size_at_most(path: &Path, limit: u64, label: &str) -> Result<u64> {
    let size = std::fs::metadata(path)
        .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("{label} unreadable: {e}")))?
        .len();
    if size > limit {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "{label} exceeds safety limit of {} MiB",
                limit / (1024 * 1024)
            ),
        ));
    }
    Ok(size)
}

static DURATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Duration:\s*(\d+):(\d{2}):(\d{2}(?:\.\d+)?)").unwrap());

/// Read the duration from `ffmpeg -i` (no ffprobe: the private ffmpeg arana
/// installs ships only ffmpeg). Without an output file ffmpeg exits 1 after
/// printing the input's details, so the exit code is not checked
/// (upstream `_probe_audio_duration`).
async fn probe_audio_duration(path: &Path, state_dir: &Path, deadline: Instant) -> Result<f64> {
    let ffmpeg = pipeline_ffmpeg(state_dir)?;
    let mut spec = CommandSpec::new(&ffmpeg, "ffmpeg");
    spec.args = vec![
        "-hide_banner".into(),
        "-i".into(),
        path.to_string_lossy().into_owned(),
    ];
    spec.timeout = Duration::from_secs(PROBE_TIMEOUT_SECONDS).min(remaining(deadline));
    let out = runner::run_captured(spec).await?;
    let m = DURATION_RE.captures(&out.stderr).ok_or_else(|| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "ffmpeg could not parse a valid audio duration",
        )
    })?;
    let duration: f64 = m[1].parse::<f64>().unwrap_or(0.0) * 3600.0
        + m[2].parse::<f64>().unwrap_or(0.0) * 60.0
        + m[3].parse::<f64>().unwrap_or(0.0);
    if !duration.is_finite() || duration <= 0.0 {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "ffmpeg could not parse a valid positive audio duration",
        ));
    }
    Ok(duration)
}

/// Reject audio that cannot fit within the bounded chunk budget
/// (upstream `_require_duration_within_budget`).
async fn require_duration_within_budget(
    path: &Path,
    state_dir: &Path,
    deadline: Instant,
) -> Result<f64> {
    let duration = probe_audio_duration(path, state_dir, deadline).await?;
    if duration > MAX_AUDIO_SECONDS as f64 {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "audio duration exceeds safety limit of {} minutes",
                MAX_AUDIO_SECONDS / 60
            ),
        ));
    }
    Ok(duration)
}

/// Re-encode to mono / 16kHz / 32kbps m4a — keeps most content under 25MB
/// (upstream `compress_audio`).
async fn compress_audio(
    src: &Path,
    out_dir: &Path,
    state_dir: &Path,
    deadline: Instant,
) -> Result<PathBuf> {
    let ffmpeg = pipeline_ffmpeg(state_dir)?;
    let dst = out_dir.join("compressed.m4a");
    let mut spec = CommandSpec::new(&ffmpeg, "ffmpeg");
    spec.args = vec![
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src.to_string_lossy().into_owned(),
        "-t".into(),
        MAX_AUDIO_SECONDS.to_string(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-b:a".into(),
        "32k".into(),
        dst.to_string_lossy().into_owned(),
    ];
    spec.timeout = remaining(deadline);
    let out = runner::run_captured(spec).await?;
    if !out.success() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "ffmpeg failed (exit {}): {}",
                out.code(),
                first_chars(out.stderr.trim(), 300)
            ),
        ));
    }
    Ok(dst)
}

/// Split `src` into 10-minute segments (upstream default).
async fn chunk_audio(
    src: &Path,
    out_dir: &Path,
    state_dir: &Path,
    deadline: Instant,
) -> Result<Vec<PathBuf>> {
    chunk_audio_with_segment(src, out_dir, state_dir, CHUNK_SECONDS, deadline).await
}

/// Split `src` into segments. Re-encodes each segment so cuts align to
/// keyframes (upstream `chunk_audio`).
pub async fn chunk_audio_with_segment(
    src: &Path,
    out_dir: &Path,
    state_dir: &Path,
    segment_seconds: u64,
    deadline: Instant,
) -> Result<Vec<PathBuf>> {
    if segment_seconds == 0 {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "chunk segment duration must be positive",
        ));
    }
    let possible_chunks = MAX_AUDIO_SECONDS.div_ceil(segment_seconds) as usize;
    if possible_chunks > MAX_CHUNKS {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "chunk generation safety limit is {MAX_CHUNKS}; segment duration {segment_seconds}s could create {possible_chunks} chunks"
            ),
        ));
    }
    let ffmpeg = pipeline_ffmpeg(state_dir)?;
    let pattern = out_dir.join("chunk_%03d.m4a");
    let mut spec = CommandSpec::new(&ffmpeg, "ffmpeg");
    spec.args = vec![
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src.to_string_lossy().into_owned(),
        "-t".into(),
        MAX_AUDIO_SECONDS.to_string(),
        "-f".into(),
        "segment".into(),
        "-segment_time".into(),
        segment_seconds.to_string(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-b:a".into(),
        "32k".into(),
        pattern.to_string_lossy().into_owned(),
    ];
    spec.timeout = remaining(deadline);
    let out = runner::run_captured(spec).await?;
    if !out.success() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "ffmpeg failed (exit {}): {}",
                out.code(),
                first_chars(out.stderr.trim(), 300)
            ),
        ));
    }
    let mut chunks: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with("chunk_") && n.ends_with(".m4a")
                        })
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    chunks.sort();
    if chunks.is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "ffmpeg produced no chunks",
        ));
    }
    Ok(chunks)
}

/// POST one chunk to the provider; returns `(status, body)`.
async fn post_chunk(
    bytes: Vec<u8>,
    file_name: String,
    provider: &Provider,
    opts: &TranscribeOptions,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, String)> {
    let mut form = reqwest::multipart::Form::new()
        .text("model", provider.model.to_string())
        .text("response_format", "text")
        .part(
            "file",
            reqwest::multipart::Part::bytes(bytes)
                .file_name(file_name)
                .mime_str("audio/m4a")
                .map_err(|e| {
                    ReachError::new(
                        "UPSTREAM_BROKEN",
                        format!("{}: bad mime: {e}", provider.name),
                    )
                })?,
        );
    if let Some(language) = opts.language {
        form = form.text("language", language.to_string());
    }
    if let Some(prompt) = opts.prompt {
        form = form.text("prompt", prompt.to_string());
    }
    let resp = reqwest::Client::new()
        .post(&provider.endpoint)
        .bearer_auth(&provider.key)
        .multipart(form)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            ReachError::new(
                "UPSTREAM_BROKEN",
                format!("{}: network error: {e}", provider.name),
            )
        })?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!("{}: network error: {e}", provider.name),
        )
    })?;
    Ok((status, text))
}

static RATE_WAIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"in (\d+)m").unwrap());

/// The xiaoyuzhou script's 429 wait: parse "in Nm" from the error body
/// (default 2 minutes), then N*60+30 seconds.
fn rate_limit_wait(body: &str) -> Duration {
    let minutes: u64 = RATE_WAIT_RE
        .captures(body)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(2);
    Duration::from_secs(minutes * 60 + 30)
}

/// Bound the 429 wait by the remaining deadline while keeping a full upload
/// window for the retry. Sleeping until the deadline would make the retry
/// attempt pointless, so short budgets retry immediately instead.
fn rate_limit_retry_wait(body: &str, left: Duration) -> Duration {
    rate_limit_wait(body)
        .min(Duration::from_secs(900))
        .min(left.saturating_sub(CHUNK_UPLOAD_TIMEOUT))
}

/// Transcribe one chunk via the named provider; returns the response text
/// (upstream `transcribe_chunk`, plus the script's bounded 429 retry).
async fn transcribe_chunk(
    chunk: &Path,
    provider: &Provider,
    opts: &TranscribeOptions,
    deadline: Instant,
) -> Result<String> {
    let bytes = std::fs::read(chunk).map_err(|e| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!("{}: unreadable chunk: {e}", provider.name),
        )
    })?;
    let file_name = chunk
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "chunk.m4a".into());
    let mut retried = false;
    loop {
        let (status, body) = post_chunk(
            bytes.clone(),
            file_name.clone(),
            provider,
            opts,
            CHUNK_UPLOAD_TIMEOUT.min(remaining(deadline)),
        )
        .await?;
        if status.is_success() {
            return Ok(body);
        }
        // The xiaoyuzhou script waits and retries once on HTTP 429.
        if status.as_u16() == 429 && opts.retry_rate_limited && !retried {
            retried = true;
            let wait = rate_limit_retry_wait(&body, remaining(deadline));
            tokio::time::sleep(wait).await;
            continue;
        }
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "{}: HTTP {}: {}",
                provider.name,
                status.as_u16(),
                first_chars(&body, 300)
            ),
        ));
    }
}

/// Try each provider in order; return first success or raise the last error
/// (upstream `_transcribe_with_fallback`). Auto mode without explicit
/// consent only ever receives one provider here.
async fn transcribe_with_fallback(
    chunk: &Path,
    providers: &[Provider],
    opts: &TranscribeOptions,
    deadline: Instant,
) -> Result<String> {
    let mut last_err: Option<ReachError> = None;
    for provider in providers {
        match transcribe_chunk(chunk, provider, opts, deadline).await {
            Ok(text) => return Ok(text),
            Err(e) => last_err = Some(e),
        }
    }
    let name = chunk
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Err(ReachError::new(
        "UPSTREAM_BROKEN",
        format!(
            "all providers failed for {name}: {}",
            last_err
                .map(|e| e.message)
                .unwrap_or_else(|| "no provider configured".into())
        ),
    ))
}

/// Join per-chunk transcripts (upstream `"\n".join(p for p in pieces if p)`).
pub fn join_transcripts(pieces: Vec<String>) -> String {
    pieces
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The orchestrator: URL or local file → budgets → compress/chunk → provider.
/// One deadline spans the whole pipeline; every child command and upload is
/// bounded by the time left.
pub async fn transcribe_source(
    source: &str,
    state_dir: &Path,
    deadline: Instant,
    opts: TranscribeOptions,
) -> Result<Transcription> {
    let providers = configured_providers(state_dir);
    if providers.is_empty() {
        return Err(ReachError::new("NEED_SETUP", NO_KEY_MESSAGE));
    }
    // Auto mode, no consent: only the first configured provider is used
    // (upstream `order = configured[:1]`).
    let providers = &providers[..1];

    let work = private_work_dir(state_dir)?;
    let out_dir = work.path().to_path_buf();

    let src_path = Path::new(source);
    let audio = if src_path.is_file() {
        src_path.to_path_buf()
    } else {
        download_audio(source, &out_dir, state_dir, deadline).await?
    };
    require_size_at_most(&audio, MAX_SOURCE_BYTES, "source")?;
    let duration_seconds = require_duration_within_budget(&audio, state_dir, deadline).await?;

    let compressed = compress_audio(&audio, &out_dir, state_dir, deadline).await?;
    let chunks = if std::fs::metadata(&compressed).map(|m| m.len()).unwrap_or(0) <= SIZE_LIMIT_BYTES
    {
        vec![compressed]
    } else {
        chunk_audio(&compressed, &out_dir, state_dir, deadline).await?
    };
    if chunks.len() > MAX_CHUNKS {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "audio produced {} chunks; safety limit is {MAX_CHUNKS} (~{} minutes)",
                chunks.len(),
                MAX_CHUNKS * CHUNK_SECONDS as usize / 60
            ),
        ));
    }
    let mut total = 0u64;
    for chunk in &chunks {
        let label = chunk
            .file_name()
            .map(|n| format!("chunk {}", n.to_string_lossy()))
            .unwrap_or_else(|| "chunk file".into());
        total += require_size_at_most(chunk, SIZE_LIMIT_BYTES, &label)?;
    }
    if total > MAX_TOTAL_CHUNK_BYTES {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "audio chunks total {total} bytes; safety limit is {} MiB",
                MAX_TOTAL_CHUNK_BYTES / (1024 * 1024)
            ),
        ));
    }

    let mut pieces: Vec<String> = Vec::new();
    for chunk in &chunks {
        pieces.push(transcribe_with_fallback(chunk, providers, &opts, deadline).await?);
    }
    Ok(Transcription {
        text: join_transcripts(pieces),
        duration_seconds,
    })
}

// ── Xiaoyuzhou ──

static XYZ_AUDIO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(https://media\.xyzcdn\.net/[^"]*\.(?:m4a|mp3))"#).unwrap());
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""title":"([^"]*)""#).unwrap());

/// The Chinese punctuation prompt from the upstream xiaoyuzhou script.
const XYZ_PROMPT: &str =
    "以下是一段中文普通话播客录音，请输出包含完整中文标点（，。？！：；“”‘’）的转写文本。";

/// Extract the audio URL and title from an episode page (the upstream
/// script's perl one-liners: first `media.xyzcdn.net` link, first
/// `"title":"..."`).
pub fn extract_episode(page: &str) -> Option<(String, Option<String>)> {
    let audio = XYZ_AUDIO_RE.captures(page)?.get(1)?.as_str().to_string();
    let title = TITLE_RE
        .captures(page)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    Some((audio, title))
}

/// The transcript header the upstream script writes: title, source, duration,
/// transcription time, then a `---` separator.
pub fn xyz_transcript_header(title: &str, url: &str, duration_seconds: f64) -> String {
    let total = duration_seconds.max(0.0) as u64;
    let (minutes, seconds) = (total / 60, total % 60);
    let recorded = chrono::Local::now().format("%Y-%m-%d %H:%M");
    format!(
        "# {title}\n\n来源: {url}\n时长: {minutes}分{seconds}秒\n转录时间: {recorded}\n\n---\n\n"
    )
}

async fn xiaoyuzhou_transcribe(
    params: &Value,
    timeout: Duration,
    state_dir: &Path,
) -> Result<Vec<Value>> {
    let target = require_str(params, "target")?.trim().to_string();
    if !ssrf::host_matches(&target, &["xiaoyuzhoufm.com"]) {
        return Err(bad_input("target must be a xiaoyuzhoufm.com episode link"));
    }
    // Upstream checks the environment first, then the config file.
    let groq_key = env_nonempty("GROQ_API_KEY").or_else(|| config::get(state_dir, "groq_api_key"));
    if groq_key.is_none() {
        let mut error = ReachError::new("NEED_SETUP", "no Groq API key configured");
        error.hint = "Ask the user for a free key from https://console.groq.com, then run `arana configure groq-key`.".into();
        return Err(error);
    }
    if registry::resolve("ffmpeg", state_dir).is_none() {
        let mut error = ReachError::new("NEED_SETUP", "ffmpeg is not installed");
        error.hint = "Run `arana add xiaoyuzhou`.".into();
        return Err(error);
    }

    let deadline = Instant::now() + timeout;
    // Step 1: fetch the episode page and extract the audio URL and title.
    let page_bytes = ssrf::safe_fetch(
        &target,
        MAX_PAGE_BYTES,
        remaining(deadline).min(PAGE_FETCH_TIMEOUT),
    )
    .await?;
    let page = String::from_utf8_lossy(&page_bytes);
    let (audio_url, title) = extract_episode(&page).ok_or_else(|| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            "could not extract the episode audio link",
        )
    })?;

    // Steps 2–6: download, compress/chunk and transcribe with the Chinese
    // language hint, punctuation prompt and the script's bounded 429 retry.
    let opts = TranscribeOptions {
        language: Some("zh"),
        prompt: Some(XYZ_PROMPT),
        retry_rate_limited: true,
    };
    let outcome = transcribe_source(&audio_url, state_dir, deadline, opts).await?;
    let text = xyz_transcript_header(
        title.as_deref().unwrap_or(""),
        &target,
        outcome.duration_seconds,
    ) + &outcome.text;
    Ok(vec![make_item(
        None,
        Some(target),
        None,
        None,
        Some(text),
        Value::Null,
    )])
}

// ── unit tests (pure logic; pipeline/integration tests live in tests/media.rs) ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_id_forms_a_watch_url() {
        assert_eq!(
            video_url("jNQXAC9IVRw").unwrap(),
            "https://www.youtube.com/watch?v=jNQXAC9IVRw"
        );
        assert_eq!(
            video_url("  jNQXAC9IVRw \n").unwrap(),
            "https://www.youtube.com/watch?v=jNQXAC9IVRw"
        );
    }

    #[test]
    fn video_url_accepts_youtube_hosts_and_rejects_others() {
        for target in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://m.youtube.com/watch?v=abc",
            "https://YOUTUBE.COM/shorts/x",
            "www.youtube.com/watch?v=abc",
        ] {
            assert!(video_url(target).is_ok(), "must accept {target}");
        }
        for target in [
            "https://example.com",
            "https://vimeo.com/123",
            "",
            "too short",
        ] {
            let err = video_url(target).expect_err("must reject");
            assert_eq!(err.code, "BAD_INPUT");
        }
        // A subdomain of youtube.com is fine; a lookalike domain is not.
        assert!(video_url("https://music.youtube.com/xyz").is_ok());
        assert!(video_url("https://youtube.com.evil.test/x").is_err());
    }

    #[test]
    fn iso_date_formats_upload_date() {
        assert_eq!(iso_date("20260117").as_deref(), Some("2026-01-17"));
        assert_eq!(iso_date(""), None);
        assert_eq!(iso_date("2026-1-17"), None);
        assert_eq!(iso_date("2026011a"), None);
        assert_eq!(iso_date("202601177"), None);
    }

    #[test]
    fn ytdlp_failure_taxonomy_matches_upstream() {
        // NOT_FOUND family.
        for stderr in ["ERROR: [youtube] x: Video unavailable", "Private video"] {
            let e = map_ytdlp_failure(1, stderr);
            assert_eq!(e.code, "NOT_FOUND", "for {stderr}");
        }
        // Bot-check family keeps its custom hint.
        let e = map_ytdlp_failure(1, "ERROR: Sign in to confirm you're not a bot");
        assert_eq!(e.code, "PLATFORM_BLOCKED");
        assert_eq!(
            e.hint,
            "YouTube returned a verification challenge. Stop this request; this does not establish that the public video requires login."
        );
        // Rate limit family — exact "429", not "4" + "29" anywhere.
        let e = map_ytdlp_failure(1, "HTTP Error 429: Too Many Requests");
        assert_eq!(e.code, "RATE_LIMITED");
        let e = map_ytdlp_failure(1, "gave up after 4 tries in 129 seconds");
        assert_eq!(e.code, "UPSTREAM_BROKEN", "must not misread 4/129 as 429");
        // Everything else is UPSTREAM_BROKEN with the last stderr line.
        let e = map_ytdlp_failure(2, "line one\nERROR: something else");
        assert_eq!(e.code, "UPSTREAM_BROKEN");
        assert_eq!(e.message, "yt-dlp exited 2: ERROR: something else");
        let e = map_ytdlp_failure(3, "");
        assert_eq!(e.message, "yt-dlp exited 3: no error output");
    }

    // ── vtt_to_text: ports of the upstream test_unified_entry.py cases ──

    #[test]
    fn vtt_to_text_drops_timings_tags_and_rolling_repeats() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
                   00:00:01.000 --> 00:00:02.000\n<c>hello</c> world\n\n\
                   00:00:02.000 --> 00:00:03.000\nhello world\n\n\
                   2\n00:00:03.000 --> 00:00:04.000\nnext &amp; last\n";
        assert_eq!(vtt_to_text(vtt), "hello world\nnext & last");
    }

    #[test]
    fn vtt_to_text_distinguishes_cue_payload_from_metadata() {
        let cases = [
            ("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n2026\n", "2026"),
            (
                "WEBVTT\n\ncaption-one\n00:00:00.000 --> 00:00:01.000\nhello\n",
                "hello",
            ),
            (
                "WEBVTT\n\nNOTE internal note\nnot speech\n\n00:00:00.000 --> 00:00:01.000\nhello\n",
                "hello",
            ),
            (
                "WEBVTT\n\nSTYLE\n::cue { color: red; }\n\n00:00:00.000 --> 00:00:01.000\nhello\n",
                "hello",
            ),
            (
                "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nNOTE this is spoken\nSTYLE matters\n",
                "NOTE this is spoken\nSTYLE matters",
            ),
        ];
        for (vtt, expected) in cases {
            assert_eq!(vtt_to_text(vtt), expected);
        }
    }

    #[test]
    fn vtt_to_text_accepts_standard_timing_separators_and_long_hours() {
        for timing in [
            "00:00:00.000\t-->\t00:00:01.000",
            "00:00:00.000  -->  00:00:01.000",
            "100:00:00.000 --> 100:00:01.000",
        ] {
            let vtt = format!("WEBVTT\n\n{timing}\nspoken words\n");
            assert_eq!(vtt_to_text(&vtt), "spoken words", "for timing {timing}");
        }
    }

    #[test]
    fn vtt_lang_code_and_ranking() {
        assert_eq!(vtt_lang_code("jNQXAC9IVRw.en.vtt"), Some("en"));
        assert_eq!(vtt_lang_code("jNQXAC9IVRw.zh-Hans.vtt"), Some("zh-Hans"));
        assert_eq!(vtt_lang_code("novtt"), None);
        let files = vec![
            "jNQXAC9IVRw.en.vtt".to_string(),
            "jNQXAC9IVRw.zh.vtt".to_string(),
        ];
        let (best, code) = pick_best_vtt(&files, &["zh-Hans".into(), "zh".into(), "en".into()])
            .expect("picks a file");
        assert_eq!(best, "jNQXAC9IVRw.zh.vtt");
        assert_eq!(code.as_deref(), Some("zh"));
    }

    #[test]
    fn html_unescape_handles_named_and_numeric_entities() {
        assert_eq!(html_unescape("a &amp; b"), "a & b");
        assert_eq!(html_unescape("&#65;&#x42;"), "AB");
        assert_eq!(html_unescape("lone &amp"), "lone &amp");
        assert_eq!(html_unescape("plain"), "plain");
    }

    #[test]
    fn xiaoyuzhou_page_extraction() {
        let page = "<html><script>window.__INITIAL_STATE__={\"title\":\"第 42 期：聊测试\"}</script>\n                      <audio src=\"https://media.xyzcdn.net/xyz-podcast/abc123.m4a\"></audio></html>";
        let (audio, title) = extract_episode(page).expect("extracts");
        assert_eq!(audio, "https://media.xyzcdn.net/xyz-podcast/abc123.m4a");
        assert_eq!(title.as_deref(), Some("第 42 期：聊测试"));
        assert!(extract_episode("<html>no audio here</html>").is_none());
        // mp3 links are picked up too.
        let (audio, _) = extract_episode("\"x\":\"https://media.xyzcdn.net/e.mp3\"").unwrap();
        assert_eq!(audio, "https://media.xyzcdn.net/e.mp3");
        // The script's perl regex is case-insensitive (`/i`).
        let (audio, _) = extract_episode("\"x\":\"HTTPS://MEDIA.XYZCDN.NET/E.M4A\"").unwrap();
        assert_eq!(audio, "HTTPS://MEDIA.XYZCDN.NET/E.M4A");
    }

    #[test]
    fn xiaoyuzhou_header_format_matches_the_script() {
        let header = xyz_transcript_header("标题", "https://www.xiaoyuzhoufm.com/episode/1", 125.7);
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        assert_eq!(
            header,
            format!(
                "# 标题\n\n来源: https://www.xiaoyuzhoufm.com/episode/1\n时长: 2分5秒\n转录时间: {now}\n\n---\n\n"
            )
        );
    }

    #[test]
    fn rate_limit_wait_parses_minutes_and_defaults() {
        assert_eq!(
            rate_limit_wait("rate limited; retry in 7m"),
            Duration::from_secs(7 * 60 + 30)
        );
        assert_eq!(
            rate_limit_wait("no delay info"),
            Duration::from_secs(2 * 60 + 30)
        );
        assert_eq!(rate_limit_wait("in 0m"), Duration::from_secs(30));
    }

    #[test]
    fn retry_wait_never_sleeps_past_the_deadline() {
        // Plenty of budget: the wait is capped at 900s.
        assert_eq!(
            rate_limit_retry_wait("retry in 60m", Duration::from_secs(1800)),
            Duration::from_secs(900)
        );
        assert_eq!(
            rate_limit_retry_wait("retry in 60m", Duration::from_secs(900)),
            Duration::from_secs(780)
        );
        // Less than one upload window left: retry immediately, not into the wall.
        assert_eq!(
            rate_limit_retry_wait("retry in 0m", Duration::from_secs(5)),
            Duration::ZERO
        );
    }

    #[test]
    fn private_work_dirs_are_unique_and_cleaned_up() {
        let state = tempfile::tempdir().unwrap();
        let first = private_work_dir(state.path()).unwrap();
        let second = private_work_dir(state.path()).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().is_dir());
        assert!(second.path().is_dir());
        let first_path = first.path().to_path_buf();
        drop(first);
        assert!(!first_path.exists(), "work dir removed on drop");
    }

    #[tokio::test]
    async fn chunk_budget_guard_rejects_unsafe_segments_before_ffmpeg() {
        // ceil(14400 / 599) = 25 > 24 chunks: must fail before any ffmpeg run.
        let err = chunk_audio_with_segment(
            Path::new("whatever.m4a"),
            Path::new("."),
            Path::new("."),
            CHUNK_SECONDS - 1,
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .expect_err("must reject the segment");
        assert_eq!(err.code, "UPSTREAM_BROKEN");
        assert!(err.message.contains("chunk generation safety limit is 24"));
        assert!(err.message.contains("could create 25 chunks"));
    }

    #[test]
    fn joined_transcripts_skip_empty_pieces() {
        let pieces = vec![
            "part one ".to_string(),
            "  ".to_string(),
            "part two".to_string(),
        ];
        assert_eq!(join_transcripts(pieces), "part one\npart two");
        assert_eq!(join_transcripts(vec![]), "");
    }
}
