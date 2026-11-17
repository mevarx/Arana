//! Tool registry: upstream external tools with pinned `tested` versions,
//! floors and excludes, plus resolution (private copy first, then PATH).
//!
//! Mirrors upstream `runtime/tools.py`: tools with `tested` are installed at
//! that version; each tool may set `floor` (oldest accepted) and `exclude`
//! (known-bad). Private installs live under `state_dir/tools/<name>/` and
//! never on the user's PATH; the user's own copy, when found on PATH, is used
//! as-is and never downgraded.

use reach_core::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Kind of registry entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    /// Pinned release binary downloaded and extracted under `tools/<name>/`.
    Binary,
    /// Executable used as found (private copy first, then the system PATH).
    Command,
}

/// One registry entry. Mirrors upstream `Tool` (tested/floor/exclude).
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub kind: ToolKind,
    /// Executables this tool provides.
    pub commands: &'static [&'static str],
    /// Reproducible default version for newly installed tools.
    pub tested: Option<&'static str>,
    /// Oldest acceptable version.
    pub floor: Option<&'static str>,
    /// Known-bad versions.
    pub exclude: &'static [&'static str],
    /// Environment variable that overrides resolution (tests/operators).
    pub env_bin: &'static str,
}

/// The registry. obscura is the M1.5 pinned web-backend binary
/// (h4ckf0r0day/obscura v0.2.7, Windows stealth archive, sha256-verified at
/// install); yt-dlp matches upstream's `constraints.txt` pin; ffmpeg has no
/// pinned version (system copies vary and are used as-is).
static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "obscura",
        kind: ToolKind::Binary,
        commands: &["obscura"],
        tested: Some("0.2.7"),
        floor: Some("0.2.0"),
        exclude: &[],
        env_bin: "ARANA_OBSCURA_BIN",
    },
    ToolSpec {
        name: "yt-dlp",
        kind: ToolKind::Command,
        commands: &["yt-dlp"],
        tested: Some("2026.8.19"),
        floor: Some("2026.1.1"),
        exclude: &[],
        env_bin: "ARANA_YTDLP_BIN",
    },
    ToolSpec {
        name: "ffmpeg",
        kind: ToolKind::Command,
        commands: &["ffmpeg"],
        tested: None,
        floor: None,
        exclude: &[],
        env_bin: "ARANA_FFMPEG_BIN",
    },
];

/// The registry itself.
pub fn tools() -> &'static [ToolSpec] {
    TOOLS
}

/// Look up a tool by name.
pub fn tool(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Private install directory for a tool (`state_dir/tools/<name>/bin`).
pub fn tool_bin_dir(name: &str, state_dir: &Path) -> PathBuf {
    state_dir.join("tools").join(name).join("bin")
}

fn exe_name(command: &str) -> String {
    if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

/// Resolve a command name to an executable path:
/// 1. `ARANA_*_BIN` env override (tests, operators with custom builds),
/// 2. our private install under `state_dir/tools/<tool>/bin/`,
/// 3. the system PATH (`which`).
///
/// The user's own copy is used as-is, never downgraded (FR-TLS-2).
pub fn resolve(command: &str, state_dir: &Path) -> Option<PathBuf> {
    for spec in TOOLS {
        if !spec.commands.contains(&command) {
            continue;
        }
        if let Ok(path) = std::env::var(spec.env_bin) {
            let candidate = PathBuf::from(&path);
            if !path.is_empty() && candidate.exists() {
                return Some(candidate);
            }
        }
        let private = tool_bin_dir(spec.name, state_dir).join(exe_name(command));
        if private.exists() {
            return Some(private);
        }
    }
    which::which(command).ok()
}

/// Parse `x.y.z` from arbitrary text (upstream `_version_tuple`).
pub fn version_tuple(text: &str) -> Option<(u64, u64, u64)> {
    static VERSION_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").unwrap());
    let captures = VERSION_RE.captures(text)?;
    Some((
        captures[1].parse().ok()?,
        captures[2].parse().ok()?,
        captures[3].parse().ok()?,
    ))
}

/// Is `version` within the tool's floor/exclude limits?
pub fn allowed(spec: &ToolSpec, version: &str) -> bool {
    let Some(v) = version_tuple(version) else {
        return false;
    };
    if let Some(floor) = spec.floor {
        if let Some(f) = version_tuple(floor) {
            if v < f {
                return false;
            }
        }
    }
    !spec.exclude.iter().any(|bad| version_tuple(bad) == Some(v))
}

/// Record a tool in `state.json` under `tools` with owner/scope (ledger).
/// Returns `{"tool": name, ...entry}` like upstream `record()`.
pub fn record(state_dir: &Path, name: &str, entry: Value) -> Result<Value> {
    let store = reach_core::state::Store::new(state_dir);
    store.mutate(|v| {
        if !v["tools"].is_object() {
            v["tools"] = Value::Object(Default::default());
        }
        v["tools"][name] = entry.clone();
    })?;
    let mut out = serde_json::json!({"tool": name});
    if let (Some(dst), Some(src)) = (out.as_object_mut(), entry.as_object()) {
        for (key, value) in src {
            dst.insert(key.clone(), value.clone());
        }
    }
    Ok(out)
}
