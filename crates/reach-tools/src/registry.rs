//! Tool registry: upstream external tools with pinned `tested` versions,
//! floors and excludes, plus resolution (private copy first, then PATH).
//!
//! Mirrors upstream `runtime/tools.py`: tools with `tested` are installed at
//! that version; each tool may set `floor` (oldest accepted) and `exclude`
//! (known-bad). Private installs live under `state_dir/tools/<name>/` and
//! never on the user's PATH; the user's own copy, when found on PATH, is used
//! as-is and never downgraded.
//!
//! Kinds and private layouts:
//! - `Binary`: pinned release under `tools/<name>/bin/` (obscura).
//! - `Command`: used where found, never installed (legacy registry entries).
//! - `Pip`: own venv under `tools/<name>/venv/` (bili-cli, boss-agent-cli,
//!   cloakbrowser, yt-dlp).
//! - `Npm`: local prefix `tools/<name>/node_modules/` (non-singletons) or the
//!   shared `tools/npm-global/` prefix used by singletons (OpenCLI).
//! - `Node`, `Uv`, `Ffmpeg`, `Chrome`: machine prerequisites; the system copy
//!   is preferred, a private copy is installed only when there is none.

use crate::config;
use reach_core::Result;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Kind of registry entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    /// Pinned release binary downloaded and extracted under `tools/<name>/`.
    Binary,
    /// Executable used as found (private copy first, then the system PATH).
    Command,
    /// Python package in a private venv under `tools/<name>/venv/`.
    Pip,
    /// npm package under a private prefix (local or the shared `npm-global`).
    Npm,
    /// Node.js runtime prerequisite (`node`, `npm`, `npx`).
    Node,
    /// uv prerequisite (`uv`, `uvx`).
    Uv,
    /// ffmpeg prerequisite (system copy, else the `imageio-ffmpeg` wheel).
    Ffmpeg,
    /// Chrome/Chromium prerequisite (system copy, else Chrome for Testing).
    Chrome,
}

impl ToolKind {
    /// Stable string used in ledger records and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Command => "command",
            Self::Pip => "pip",
            Self::Npm => "npm",
            Self::Node => "node",
            Self::Uv => "uv",
            Self::Ffmpeg => "ffmpeg",
            Self::Chrome => "chrome",
        }
    }
}

/// One registry entry. Mirrors upstream `Tool` (tested/floor/exclude).
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub kind: ToolKind,
    /// pip requirement / npm package name (`""` for prerequisites).
    pub package: &'static str,
    /// Executables this tool provides.
    pub commands: &'static [&'static str],
    /// Only one copy can exist per machine: OpenCLI owns a fixed daemon port
    /// and a Chrome extension, so a user copy is reused instead of shadowed.
    pub singleton: bool,
    /// Reproducible default version for newly installed tools.
    pub tested: Option<&'static str>,
    /// Oldest acceptable version.
    pub floor: Option<&'static str>,
    /// Known-bad versions.
    pub exclude: &'static [&'static str],
    /// Tools that must be present first.
    pub needs: &'static [&'static str],
    /// Environment variable that overrides resolution (tests/operators).
    pub env_bin: &'static str,
}

/// The registry. obscura is the pinned web-backend binary
/// (h4ckf0r0day/obscura v0.2.7, sha256-verified at install). yt-dlp matches
/// upstream `constraints.txt` (2026.8.19); bili-cli / cloakbrowser / OpenCLI
/// match upstream `runtime/tools.py` tested pins. boss-agent-cli is a pinned
/// GitHub archive, not the unrelated PyPI project of the same name.
static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "obscura",
        kind: ToolKind::Binary,
        package: "",
        commands: &["obscura"],
        singleton: false,
        tested: Some("0.2.7"),
        floor: Some("0.2.0"),
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_OBSCURA_BIN",
    },
    ToolSpec {
        name: "yt-dlp",
        kind: ToolKind::Pip,
        package: "yt-dlp",
        commands: &["yt-dlp"],
        singleton: false,
        tested: Some("2026.8.19"),
        floor: Some("2026.1.1"),
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_YTDLP_BIN",
    },
    ToolSpec {
        name: "bili-cli",
        kind: ToolKind::Pip,
        package: "bilibili-cli",
        commands: &["bili"],
        singleton: false,
        tested: Some("0.6.2"),
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_BILI_BIN",
    },
    ToolSpec {
        name: "boss-agent-cli",
        kind: ToolKind::Pip,
        package: "boss-agent-cli @ https://github.com/can4hou6joeng4/boss-agent-cli/archive/4c991b77086a203173bf08a4cb64a23af6514fe6.zip",
        commands: &["boss"],
        singleton: false,
        tested: None,
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_BOSS_BIN",
    },
    ToolSpec {
        name: "cloakbrowser",
        kind: ToolKind::Pip,
        package: "cloakbrowser",
        commands: &["cloakbrowser"],
        singleton: false,
        tested: Some("0.6.3"),
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_CLOAKBROWSER_BIN",
    },
    ToolSpec {
        name: "opencli",
        kind: ToolKind::Npm,
        package: "@jackwener/opencli",
        commands: &["opencli"],
        singleton: true,
        tested: Some("2.0.3"),
        floor: Some("1.8.7"),
        exclude: &["1.8.8"],
        needs: &["node"],
        env_bin: "ARANA_OPENCLI_BIN",
    },
    ToolSpec {
        name: "node",
        kind: ToolKind::Node,
        package: "",
        commands: &["node", "npm", "npx"],
        singleton: false,
        tested: None,
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_NODE_BIN",
    },
    ToolSpec {
        name: "uv",
        kind: ToolKind::Uv,
        package: "",
        commands: &["uv", "uvx"],
        singleton: false,
        tested: None,
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_UV_BIN",
    },
    ToolSpec {
        name: "ffmpeg",
        kind: ToolKind::Ffmpeg,
        package: "imageio-ffmpeg",
        commands: &["ffmpeg"],
        singleton: false,
        tested: None,
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_FFMPEG_BIN",
    },
    ToolSpec {
        name: "chrome",
        kind: ToolKind::Chrome,
        package: "",
        commands: &[],
        singleton: false,
        tested: None,
        floor: None,
        exclude: &[],
        needs: &[],
        env_bin: "ARANA_CHROME_BIN",
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

/// Look up the tool that provides `command`.
pub fn by_command(command: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|spec| spec.commands.contains(&command))
}

/// Private install directory for a tool (`state_dir/tools/<name>`).
pub fn tool_root(name: &str, state_dir: &Path) -> PathBuf {
    state_dir.join("tools").join(name)
}

/// Private install directory for a tool (`state_dir/tools/<name>/bin`).
pub fn tool_bin_dir(name: &str, state_dir: &Path) -> PathBuf {
    tool_root(name, state_dir).join("bin")
}

/// Shared npm global prefix for singletons we install.
pub fn npm_global_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("tools").join("npm-global")
}

/// Node private install root (`tools/node/current/...`).
pub fn node_current_dir(state_dir: &Path) -> PathBuf {
    tool_root("node", state_dir).join("current")
}

/// Directory holding the private `node`/`npm`/`npx` executables.
pub fn node_bin_dir(state_dir: &Path) -> PathBuf {
    let base = node_current_dir(state_dir);
    if cfg!(windows) {
        base
    } else {
        base.join("bin")
    }
}

/// Directory holding the private `uv`/`uvx` executables.
pub fn uv_bin_dir(state_dir: &Path) -> PathBuf {
    tool_root("uv", state_dir).join("bin")
}

/// Directory holding the private `ffmpeg` executable.
pub fn ffmpeg_bin_dir(state_dir: &Path) -> PathBuf {
    tool_root("ffmpeg", state_dir).join("bin")
}

/// Directory holding the private Chrome for Testing install.
pub fn chrome_current_dir(state_dir: &Path) -> PathBuf {
    tool_root("chrome", state_dir).join("current")
}

/// Python venv for a pip tool.
pub fn venv_dir(name: &str, state_dir: &Path) -> PathBuf {
    tool_root(name, state_dir).join("venv")
}

/// `Scripts` on Windows, `bin` elsewhere.
pub fn venv_bin_dir(name: &str, state_dir: &Path) -> PathBuf {
    let venv = venv_dir(name, state_dir);
    if cfg!(windows) {
        venv.join("Scripts")
    } else {
        venv.join("bin")
    }
}

/// The venv interpreter we created (used for pip installs and repairs).
pub fn venv_python(name: &str, state_dir: &Path) -> PathBuf {
    venv_bin_dir(name, state_dir).join(exe_name("python"))
}

/// The shared npm prefix's executable directory (Windows: the prefix itself).
pub fn npm_global_bin_dir(state_dir: &Path) -> PathBuf {
    let prefix = npm_global_dir(state_dir);
    if cfg!(windows) {
        prefix
    } else {
        prefix.join("bin")
    }
}

fn exe_name(command: &str) -> String {
    if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

/// Regular npm/npx CLI script under the private Node root
/// (`lib/node_modules/npm/bin/{npm,npx}-cli.js`).
fn node_npm_cli(command: &str, state_dir: &Path) -> Option<PathBuf> {
    let script = match command {
        "npm" => "npm-cli.js",
        "npx" => "npx-cli.js",
        _ => return None,
    };
    let path = node_current_dir(state_dir)
        .join("lib")
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join(script);
    path.is_file().then_some(path)
}

/// Candidate private launcher paths for `command`, most preferred first.
fn private_candidates(spec: &ToolSpec, command: &str, state_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match spec.kind {
        ToolKind::Binary | ToolKind::Command => {
            out.push(tool_bin_dir(spec.name, state_dir).join(exe_name(command)));
        }
        ToolKind::Pip | ToolKind::Ffmpeg => {
            out.push(venv_bin_dir(spec.name, state_dir).join(exe_name(command)));
            if spec.kind == ToolKind::Ffmpeg {
                // imageio-ffmpeg's launcher is copied here as well; keep the
                // pre-existing `tools/<name>/bin` layout resolvable.
                out.push(tool_bin_dir(spec.name, state_dir).join(exe_name(command)));
            }
        }
        ToolKind::Npm => {
            let prefix = if spec.singleton {
                npm_global_dir(state_dir)
            } else {
                tool_root(spec.name, state_dir)
            };
            let local_bin = prefix.join("node_modules").join(".bin");
            if cfg!(windows) {
                out.push(local_bin.join(format!("{command}.cmd")));
                out.push(local_bin.join(format!("{command}.exe")));
                out.push(local_bin.join(command));
                if spec.singleton {
                    out.push(npm_global_bin_dir(state_dir).join(format!("{command}.cmd")));
                }
            } else {
                out.push(local_bin.join(command));
            }
        }
        ToolKind::Node => {
            let dir = node_bin_dir(state_dir);
            if cfg!(windows) {
                out.push(dir.join(if command == "node" {
                    "node.exe".to_string()
                } else {
                    format!("{command}.cmd")
                }));
            } else {
                out.push(dir.join(command));
            }
            // Official Unix Node tarballs provide npm/npx only as symlink
            // shims into lib/node_modules/npm. The opt-in regular-member
            // extractor skips those links, so resolve the regular CLI scripts.
            if let Some(cli) = node_npm_cli(command, state_dir) {
                out.push(cli);
            }
        }
        ToolKind::Uv => {
            out.push(uv_bin_dir(state_dir).join(exe_name(command)));
        }
        ToolKind::Chrome => {}
    }
    out
}

/// The private launcher for `command`, if this tool installs one.
pub fn private_bin(spec: &ToolSpec, command: &str, state_dir: &Path) -> Option<PathBuf> {
    private_candidates(spec, command, state_dir)
        .into_iter()
        .find(|path| path.is_file())
}

/// Configuration override for a command (`tool_<command>_bin`), if it points
/// at an existing file. Config-first keeps tests and operators independent of
/// process-global environment variables.
fn config_override(spec: &ToolSpec, command: &str, state_dir: &Path) -> Option<PathBuf> {
    let mut keys = vec![format!("tool_{command}_bin")];
    if spec.name != command {
        keys.push(format!("tool_{}_bin", spec.name));
    }
    for key in keys {
        if let Some(value) = config::get(state_dir, &key) {
            let path = PathBuf::from(value);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Resolve a command name to an executable path:
/// 1. `ARANA_*_BIN` env override (tests, operators with custom builds),
/// 2. `tool_<command>_bin` from config.yaml (private mirrors/operators),
/// 3. our private install,
/// 4. the system PATH (`which`).
///
/// Singletons (OpenCLI) prefer the copy already on PATH so a user's own
/// install is reused; only when nothing is on PATH do we use our shared npm
/// prefix. The user's copy is never modified.
pub fn resolve(command: &str, state_dir: &Path) -> Option<PathBuf> {
    let Some(spec) = by_command(command) else {
        return which::which(command).ok();
    };
    if let Ok(path) = std::env::var(spec.env_bin) {
        let candidate = PathBuf::from(&path);
        if !path.is_empty() && candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(path) = config_override(spec, command, state_dir) {
        return Some(path);
    }
    if spec.singleton {
        if let Ok(found) = which::which(command) {
            return Some(found);
        }
        return private_bin(spec, command, state_dir);
    }
    if let Some(private) = private_bin(spec, command, state_dir) {
        return Some(private);
    }
    which::which(command).ok()
}

/// Private bin directories that exist (node, the shared npm prefix, uv, ffmpeg).
pub fn bin_dirs(state_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        node_bin_dir(state_dir),
        npm_global_bin_dir(state_dir),
        uv_bin_dir(state_dir),
        ffmpeg_bin_dir(state_dir),
    ];
    dirs.retain(|dir| dir.is_dir());
    dirs.sort();
    dirs.dedup();
    dirs
}

fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Environment overrides a caller must apply to a child process that runs
/// `command`: `PATH` gets the private runtime bin directories (node, the
/// shared npm prefix, uv, ffmpeg) **prepended**; the process environment is
/// never mutated.
///
/// This is what makes a private `opencli.cmd` work on a machine without a
/// system Node: npm's shim calls `node`, which then resolves inside our
/// private Node install. Existing PATH entries are preserved after ours.
pub fn execution_env(command: &str, state_dir: &Path) -> Vec<(String, String)> {
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(spec) = by_command(command) {
        match spec.kind {
            ToolKind::Pip | ToolKind::Ffmpeg => {
                extra.push(venv_bin_dir(spec.name, state_dir));
            }
            ToolKind::Npm => {
                if spec.singleton {
                    extra.push(npm_global_bin_dir(state_dir));
                } else {
                    extra.push(
                        tool_root(spec.name, state_dir)
                            .join("node_modules")
                            .join(".bin"),
                    );
                }
                // A private npm shim needs a private node when the host has none.
                extra.push(node_bin_dir(state_dir));
            }
            ToolKind::Node => extra.push(node_bin_dir(state_dir)),
            ToolKind::Uv => extra.push(uv_bin_dir(state_dir)),
            ToolKind::Binary => extra.push(tool_bin_dir(spec.name, state_dir)),
            ToolKind::Command | ToolKind::Chrome => {}
        }
    }
    extra.retain(|dir| dir.is_dir());
    if extra.is_empty() {
        return Vec::new();
    }
    let mut seen: std::collections::HashSet<String> = extra.iter().map(|p| path_key(p)).collect();
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut parts = extra;
    for part in std::env::split_paths(&current) {
        if !part.as_os_str().is_empty() && seen.insert(path_key(&part)) {
            parts.push(part);
        }
    }
    match std::env::join_paths(parts) {
        Ok(joined) => vec![("PATH".to_string(), joined.to_string_lossy().into_owned())],
        Err(_) => Vec::new(),
    }
}

/// One fully-resolved way to start a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    /// Arguments that must come before the caller's arguments (Windows batch
    /// shims are started through `cmd.exe /C call`).
    pub args_prefix: Vec<String>,
}

fn is_batch(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
        .unwrap_or(false)
}

fn cmd_exe() -> PathBuf {
    if let Some(comspec) = std::env::var_os("COMSPEC") {
        let path = PathBuf::from(comspec);
        if path.is_file() {
            return path;
        }
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        let path = PathBuf::from(root).join("System32").join("cmd.exe");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("cmd.exe")
}

/// Turn an executable path into something the process APIs can start. On
/// Windows a `.cmd`/`.bat` shim (npm's launchers, pip's console scripts) is
/// not a valid `CreateProcess` image; `cmd.exe /C call <shim>` is.
pub fn invocation_for_path(path: PathBuf) -> Invocation {
    if cfg!(windows) && is_batch(&path) {
        return Invocation {
            program: cmd_exe(),
            args_prefix: vec![
                "/C".to_string(),
                "call".to_string(),
                path.to_string_lossy().into_owned(),
            ],
        };
    }
    Invocation {
        program: path,
        args_prefix: Vec::new(),
    }
}

/// Describe how to start an already-resolved `path` for `command`. A private
/// npm/npx CLI script (an official Unix Node tarball whose symlink shims were
/// skipped by the regular-member extractor) is started through the private
/// `node` runtime instead of being executed directly.
pub fn invocation_for_resolved(command: &str, path: &Path, state_dir: &Path) -> Invocation {
    let cli_script = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "npm-cli.js" || name == "npx-cli.js");
    let node_runtime = if cli_script
        && matches!(
            by_command(command).map(|spec| spec.kind),
            Some(ToolKind::Node)
        ) {
        by_command(command).and_then(|spec| private_bin(spec, "node", state_dir))
    } else {
        None
    };
    if let Some(node) = node_runtime {
        return Invocation {
            program: node,
            args_prefix: vec![path.to_string_lossy().into_owned()],
        };
    }
    invocation_for_path(path.to_path_buf())
}

/// Resolve `command` and describe how to start it (`None` when missing).
pub fn resolve_invocation(command: &str, state_dir: &Path) -> Option<Invocation> {
    resolve(command, state_dir).map(|path| invocation_for_resolved(command, &path, state_dir))
}

/// Chrome/Chromium executable: config override, private install, then the
/// usual system locations and PATH names.
pub fn chrome_binary(state_dir: &Path) -> Option<PathBuf> {
    for key in ["tool_chrome_bin", "tool_chrome_exe"] {
        if let Some(value) = config::get(state_dir, key) {
            let path = PathBuf::from(value);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let private = chrome_current_dir(state_dir).join(exe_name("chrome"));
    if private.is_file() {
        return Some(private);
    }
    #[cfg(windows)]
    {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for env in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                candidates.push(
                    PathBuf::from(base)
                        .join("Google")
                        .join("Chrome")
                        .join("Application")
                        .join("chrome.exe"),
                );
            }
        }
        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        if let Ok(path) = which::which(name) {
            return Some(path);
        }
    }
    None
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

/// Distribution name from a pip requirement (`boss-agent-cli @ URL` →
/// `boss-agent-cli`).
pub fn dist_name(package: &str) -> String {
    package
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '@' | '<' | '>' | '=' | '!' | '~' | '[' | ';')
        })
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_string()
}

fn npm_range(spec: &ToolSpec) -> String {
    let mut bad: Vec<&str> = spec.exclude.to_vec();
    bad.sort_by_key(|v| version_tuple(v));
    let mut lower = spec
        .floor
        .map(|f| format!(">={f}"))
        .unwrap_or_else(|| ">=0.0.0".to_string());
    let mut parts = Vec::new();
    for version in bad {
        parts.push(format!("{lower} <{version}"));
        lower = format!(">{version}");
    }
    parts.push(lower);
    parts.join(" || ")
}

/// Version spec passed to pip/npm: the tested pin when known; otherwise the
/// floor/exclude range. Direct URL pip requirements are left untouched.
pub fn install_spec(spec: &ToolSpec) -> String {
    match spec.kind {
        ToolKind::Pip | ToolKind::Ffmpeg => {
            if let Some(tested) = spec.tested {
                return format!("{}=={tested}", spec.package);
            }
            if spec.package.contains(" @ ") {
                return spec.package.to_string();
            }
            let mut constraints = Vec::new();
            if let Some(floor) = spec.floor {
                constraints.push(format!(">={floor}"));
            }
            for version in spec.exclude {
                constraints.push(format!("!={version}"));
            }
            if constraints.is_empty() {
                spec.package.to_string()
            } else {
                format!("{}{}", spec.package, constraints.join(","))
            }
        }
        ToolKind::Npm => {
            if let Some(tested) = spec.tested {
                return format!("{}@{tested}", spec.package);
            }
            let range = npm_range(spec);
            if range == ">=0.0.0" {
                spec.package.to_string()
            } else {
                format!("{}@{range}", spec.package)
            }
        }
        _ => spec.package.to_string(),
    }
}

/// A ledger path is owned only when it is a normal relative path below
/// `tools/<tool>/`. Forged absolute or `..` entries are refused.
pub fn owned_tool_relative(recorded: &str, name: &str) -> Option<PathBuf> {
    let path = Path::new(recorded);
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(part)) if part == "tools" => {}
        _ => return None,
    }
    match components.next() {
        Some(Component::Normal(part)) if part == name => {}
        _ => return None,
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => return None,
        }
    }
    Some(relative)
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
