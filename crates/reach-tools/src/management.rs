//! Private tool management: idempotent installs/repairs, prerequisite reuse,
//! version inspection, refresh-to-tested and safe removal.
//!
//! The public surface the CLI (and the OpenCLI channel adapters) consume:
//!
//! ```ignore
//! management::ensure_tool(name, root, timeout).await?;  // install/repair
//! management::inspect_tool(name, root).await?;          // never errors when missing
//! management::update_tool(name, root, timeout).await?;  // refresh to tested
//! management::remove_tool(name, root)?;                 // only arana-owned dirs
//! ```
//!
//! Everything installed lives under `root/tools/`; `state.json`'s `tools`
//! ledger records `owner: "arana"` for what we created. Remove refuses to
//! delete user/system copies and ignores forged ledger paths (the canonical
//! path is always recomputed from the registry).
//!
//! Prerequisites (node, uv, ffmpeg, chrome) use the system copy when one is
//! present; otherwise a private copy is installed into `root/tools/<name>/`
//! without admin rights or a global PATH change. `registry::execution_env`
//! supplies the child-only PATH that makes a private npm shim find a private
//! Node.

use crate::{config, download, fsguard, obscura, registry, ssrf};
use reach_core::{ReachError, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Owner value for tools arana installed.
const OURS: &str = "arana";
/// Registry downloads are capped well below the archive extractor's limits.
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
/// Node/uv release metadata is tiny.
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;

fn bad_input(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}

fn need_setup(message: impl Into<String>) -> ReachError {
    ReachError::new("NEED_SETUP", message)
}

fn upstream_broken(message: impl Into<String>) -> ReachError {
    ReachError::new("UPSTREAM_BROKEN", message)
}

fn timeout_error(stage: &str) -> ReachError {
    ReachError::new("TIMEOUT", format!("{stage} timed out"))
}

fn spec(name: &str) -> Result<&'static registry::ToolSpec> {
    registry::tool(name).ok_or_else(|| bad_input(format!("unknown tool: {name}")))
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn dir_is_non_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn path_under(path: &Path, root: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/");
    let root = root.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        path.to_lowercase()
            .starts_with(&format!("{}/", root.to_lowercase()))
    } else {
        path.starts_with(&format!("{root}/"))
    }
}

/// A whole-operation deadline shared by downloads and child processes.
struct Deadline {
    end: Instant,
}

impl Deadline {
    fn new(timeout: Duration) -> Self {
        Self {
            end: Instant::now() + timeout,
        }
    }

    fn remaining(&self) -> Result<Duration> {
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(timeout_error("tool operation"))
        } else {
            Ok(left)
        }
    }
}

fn install_root(spec: &registry::ToolSpec, root: &Path) -> PathBuf {
    if spec.singleton {
        registry::npm_global_dir(root)
    } else {
        registry::tool_root(spec.name, root)
    }
}

fn private_ok(spec: &registry::ToolSpec, root: &Path) -> bool {
    !spec.commands.is_empty()
        && spec
            .commands
            .iter()
            .all(|command| registry::private_bin(spec, command, root).is_some())
}

/// A usable copy outside our private tree (system PATH or operator override).
fn system_present(spec: &registry::ToolSpec, root: &Path) -> bool {
    !spec.commands.is_empty()
        && spec.commands.iter().all(|command| {
            registry::resolve(command, root)
                .map(|path| !path_under(&path, &registry::tool_root(spec.name, root)))
                .unwrap_or(false)
        })
}

/// Is every command usable (private or system)?
fn present(spec: &registry::ToolSpec, root: &Path) -> bool {
    if spec.kind == registry::ToolKind::Chrome {
        return registry::chrome_binary(root).is_some();
    }
    !spec.commands.is_empty()
        && spec
            .commands
            .iter()
            .all(|command| registry::resolve(command, root).is_some())
}

/// Pip tools carrying a floor are pinned private replacements (yt-dlp): a
/// system copy inside the limits satisfies ensure/update as-is, while an
/// out-of-limits copy triggers a private pinned install instead of a global
/// upgrade.
fn pip_reuses_system(spec: &registry::ToolSpec) -> bool {
    spec.kind == registry::ToolKind::Pip && spec.floor.is_some()
}

/// Run one installer child with a private, hidden environment. `.cmd`/`.bat`
/// launchers are started through `cmd.exe /C call` (npm/pip shims).
async fn run_program(
    program: &Path,
    args: &[String],
    root: &Path,
    timeout: Duration,
    label: &str,
    extra_env: &[(String, String)],
) -> Result<String> {
    let invocation = registry::invocation_for_path(program.to_path_buf());
    let command_name = program
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    run_invocation(
        &invocation,
        &command_name,
        args,
        root,
        timeout,
        label,
        extra_env,
    )
    .await
}

/// Run one resolved invocation with a private, hidden environment. `.cmd`/
/// `.bat` launchers are started through `cmd.exe /C call` (npm/pip shims).
async fn run_invocation(
    invocation: &registry::Invocation,
    env_command: &str,
    args: &[String],
    root: &Path,
    timeout: Duration,
    label: &str,
    extra_env: &[(String, String)],
) -> Result<String> {
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.args_prefix).args(args);
    command.kill_on_drop(true);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    // Private runtime dirs are prepended for this child only. The parent
    // process environment is never mutated. `env_command` is the tool whose
    // runtime the child needs (npm's CLI script needs the private node), not
    // the cmd.exe wrapper used for batch shims.
    if registry::by_command(env_command).is_some() {
        for (key, value) in registry::execution_env(env_command, root) {
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| timeout_error(label))?;
    let output = child.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            need_setup(format!(
                "{label} could not start {}: {error}",
                invocation.program.display()
            ))
        } else {
            ReachError::from(error)
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr
            .lines()
            .rev()
            .chain(stdout.lines().rev())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no error output");
        return Err(upstream_broken(format!("{label} failed: {last}")));
    }
    Ok(stdout)
}

fn install_env(root: &Path) -> Result<Vec<(String, String)>> {
    let cache = root.join("cache").join("downloads");
    reach_core::state::private_dir(&cache)?;
    Ok(vec![
        (
            "PIP_CACHE_DIR".to_string(),
            cache.join("pip").to_string_lossy().into_owned(),
        ),
        (
            "UV_CACHE_DIR".to_string(),
            cache.join("uv").to_string_lossy().into_owned(),
        ),
        (
            "npm_config_cache".to_string(),
            cache.join("npm").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            cache.to_string_lossy().into_owned(),
        ),
        (
            "npm_config_update_notifier".to_string(),
            "false".to_string(),
        ),
    ])
}

fn resolve_python(root: &Path) -> Result<PathBuf> {
    if let Some(value) = config::get(root, "python_bin") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
    }
    for name in ["python3", "python"] {
        if let Ok(path) = which::which(name) {
            return Ok(path);
        }
    }
    Err(need_setup(
        "Python is required to create a private venv; install Python 3 and retry",
    ))
}

fn normalize_dist(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '-' | '.' | '_') {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Version from the installed `*.dist-info` metadata (no process spawn).
fn pip_dist_version(spec: &registry::ToolSpec, root: &Path) -> Option<String> {
    let venv = registry::venv_dir(spec.name, root);
    let mut sites = Vec::new();
    if cfg!(windows) {
        sites.push(venv.join("Lib").join("site-packages"));
    } else {
        let lib = venv.join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib) {
            for entry in entries.flatten() {
                let site = entry.path().join("site-packages");
                if site.is_dir() {
                    sites.push(site);
                }
            }
        }
    }
    let want = normalize_dist(&registry::dist_name(spec.package));
    for site in sites {
        let Ok(entries) = std::fs::read_dir(&site) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            let Some(stem) = name.strip_suffix(".dist-info") else {
                continue;
            };
            let Some((dist, version)) = stem.rsplit_once('-') else {
                continue;
            };
            if normalize_dist(dist) == want {
                return Some(version.to_string());
            }
        }
    }
    None
}

/// Version from a local/global npm prefix's `package.json` metadata.
fn npm_package_version(prefix: &Path, package: &str) -> Option<String> {
    for base in [
        prefix.join("node_modules"),
        prefix.join("lib").join("node_modules"),
    ] {
        let manifest = base.join(package).join("package.json");
        let Ok(text) = std::fs::read_to_string(manifest) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(version) = value["version"].as_str() {
                return Some(version.to_string());
            }
        }
    }
    None
}

fn private_meta_version(spec: &registry::ToolSpec, root: &Path) -> Option<String> {
    match spec.kind {
        registry::ToolKind::Pip | registry::ToolKind::Ffmpeg => pip_dist_version(spec, root),
        registry::ToolKind::Npm => npm_package_version(&install_root(spec, root), spec.package),
        _ => None,
    }
}

fn version_args(spec: &registry::ToolSpec) -> Option<&'static str> {
    match spec.kind {
        registry::ToolKind::Chrome => None,
        registry::ToolKind::Ffmpeg => Some("-version"),
        _ => Some("--version"),
    }
}

async fn probe_version(
    program: &Path,
    spec: &registry::ToolSpec,
    root: &Path,
    budget: Duration,
) -> Option<String> {
    let args = version_args(spec)?;
    let invocation = registry::invocation_for_resolved(
        spec.commands.first().copied().unwrap_or(""),
        program,
        root,
    );
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.args_prefix).arg(args);
    command.kill_on_drop(true);
    for (key, value) in registry::execution_env(spec.commands.first().copied().unwrap_or(""), root)
    {
        command.env(key, value);
    }
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
    let output = tokio::time::timeout(budget.min(Duration::from_secs(20)), command.output())
        .await
        .ok()?
        .ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    registry::version_tuple(&text).map(|(a, b, c)| format!("{a}.{b}.{c}"))
}

async fn resolved_version(
    spec: &registry::ToolSpec,
    root: &Path,
    budget: Duration,
) -> Option<String> {
    if let Some(version) = private_meta_version(spec, root) {
        return Some(version);
    }
    let command = spec.commands.first()?;
    let path = registry::resolve(command, root)?;
    probe_version(&path, spec, root, budget).await
}

fn entry_for(
    spec: &registry::ToolSpec,
    version: Option<&str>,
    scope: &str,
    path: &str,
    prefix: Option<&str>,
) -> Value {
    let mut entry = json!({
        "owner": OURS,
        "scope": scope,
        "version": version,
        "kind": spec.kind.as_str(),
        "path": path,
        "commands": spec.commands,
        "installed_at": now(),
    });
    if let Some(prefix) = prefix {
        entry["prefix"] = json!(prefix);
    }
    entry
}

fn record_retry(root: &Path, name: &str, entry: Value) -> Result<Value> {
    let mut last = None;
    for _ in 0..100 {
        match registry::record(root, name, entry.clone()) {
            Ok(record) => return Ok(record),
            Err(error) if error.code == "BUSY" => {
                std::thread::sleep(Duration::from_millis(50));
                last = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        ReachError::new("BUSY", "Reach state is being updated by another command")
    }))
}

fn drop_record(root: &Path, name: &str) -> Result<()> {
    let store = reach_core::state::Store::new(root);
    let mut last = None;
    for _ in 0..100 {
        let attempt = store.mutate(|value| {
            if let Some(tools) = value["tools"].as_object_mut() {
                tools.remove(name);
            }
        });
        match attempt {
            Ok(()) => return Ok(()),
            Err(error) if error.code == "BUSY" => {
                std::thread::sleep(Duration::from_millis(50));
                last = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        ReachError::new("BUSY", "Reach state is being updated by another command")
    }))
}

fn merged_record(name: &str, entry: &Value) -> Value {
    let mut out = json!({"tool": name});
    if let (Some(dst), Some(src)) = (out.as_object_mut(), entry.as_object()) {
        for (key, value) in src {
            dst.insert(key.clone(), value.clone());
        }
    }
    out
}

/// Make sure `name` is installed inside its limits. Idempotent: an intact
/// private install at an allowed version is returned without re-downloading.
/// User/system copies are reused and never modified; a user copy outside the
/// floor/exclude limits is reported as `NEED_SETUP` (ask before upgrading it).
pub async fn ensure_tool(name: &str, root: &Path, timeout: Duration) -> Result<Value> {
    let spec = spec(name)?;
    if spec.name == "obscura" {
        return obscura::install_obscura(root).await;
    }
    let deadline = Deadline::new(timeout);
    let _lock =
        reach_core::state::FileLock::wait(root, &format!("tool-install-{name}"), timeout).await?;
    for dependency in spec.needs {
        let remaining = deadline.remaining()?;
        Box::pin(ensure_tool(dependency, root, remaining)).await?;
    }
    ensure_locked(spec, root, &deadline, false).await
}

async fn ensure_locked(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
    force: bool,
) -> Result<Value> {
    let store = reach_core::state::Store::new(root);
    let entry = store.load()?["tools"][spec.name].clone();
    let owned = entry["owner"] == OURS;
    let private_ok = private_ok(spec, root);

    if spec.kind == registry::ToolKind::Chrome {
        return ensure_chrome(spec, root, deadline, &entry, force).await;
    }

    // Prerequisites: a system copy is used as-is; only a missing one is
    // installed privately.
    if matches!(
        spec.kind,
        registry::ToolKind::Node | registry::ToolKind::Uv | registry::ToolKind::Ffmpeg
    ) && !private_ok
        && !force
        && system_present(spec, root)
    {
        let version = resolved_version(spec, root, deadline.remaining()?).await;
        return Ok(json!({
            "tool": spec.name,
            "owner": "system",
            "scope": "system",
            "version": version,
            "kind": spec.kind.as_str(),
            "commands": spec.commands,
        }));
    }

    // Pinned pip tools (yt-dlp): a system copy inside the limits is used
    // as-is. An out-of-limits copy falls through to a private pinned venv, so
    // the user's install is never upgraded or shadowed globally.
    if pip_reuses_system(spec) && !owned && !force && !private_ok && system_present(spec, root) {
        if let Some(current) = resolved_version(spec, root, deadline.remaining()?).await {
            if registry::allowed(spec, &current) {
                return Ok(json!({
                    "tool": spec.name,
                    "owner": "system",
                    "scope": "system",
                    "version": current,
                    "kind": spec.kind.as_str(),
                    "commands": spec.commands,
                }));
            }
        }
    }

    // Singleton (OpenCLI): a user copy inside the limits is used as-is; one
    // outside the limits needs explicit consent, so never silently upgrade it.
    if spec.singleton && !owned {
        if let Some(current) = resolved_version(spec, root, deadline.remaining()?).await {
            if !registry::allowed(spec, &current) {
                return Err(need_setup(format!(
                    "{} {current} is outside the supported versions (needs >= {}{}); update it yourself or keep using it via its own install",
                    spec.name,
                    spec.floor.unwrap_or("any"),
                    if spec.exclude.is_empty() {
                        String::new()
                    } else {
                        format!(", not {}", spec.exclude.join(", "))
                    }
                )));
            }
            let path = install_root(spec, root);
            let record = if entry.is_object() {
                let mut updated = entry.clone();
                updated["owner"] = json!("user");
                updated["scope"] = json!("global");
                updated["version"] = json!(current);
                updated["kind"] = json!(spec.kind.as_str());
                record_retry(root, spec.name, updated)?
            } else {
                let user = json!({
                    "owner": "user",
                    "scope": "global",
                    "version": current,
                    "kind": spec.kind.as_str(),
                    "path": path.to_string_lossy(),
                });
                record_retry(root, spec.name, user)?
            };
            return Ok(record);
        }
    }

    // A recorded version outside floor/exclude is repaired, never accepted:
    // the ledger string alone is not evidence the install is usable.
    if owned
        && !force
        && private_ok
        && entry["version"]
            .as_str()
            .is_some_and(|version| registry::allowed(spec, version))
    {
        return Ok(merged_record(spec.name, &entry));
    }

    // Refuse to write into a directory holding files we did not create.
    let target = install_root(spec, root);
    // Snapshot before installing: files present now that are not in the old
    // manifest were not created by arana and must stay unowned in the new one.
    let snapshot = snapshot_install(&target);
    if !owned && dir_is_non_empty(&target) {
        return Err(need_setup(format!(
            "Unrecorded files occupy the {} install directory; left untouched",
            spec.name
        )));
    }

    deadline.remaining()?;
    let installed = match spec.kind {
        registry::ToolKind::Pip => install_pip(spec, root, deadline, force, !private_ok).await,
        registry::ToolKind::Ffmpeg => {
            if system_present(spec, root) && !force {
                let version = resolved_version(spec, root, deadline.remaining()?).await;
                return Ok(json!({
                    "tool": spec.name,
                    "owner": "system",
                    "scope": "system",
                    "version": version,
                    "kind": spec.kind.as_str(),
                    "commands": spec.commands,
                }));
            }
            install_ffmpeg(spec, root, deadline, !private_ok).await
        }
        registry::ToolKind::Npm => install_npm(spec, root, deadline).await,
        registry::ToolKind::Node => install_node(spec, root, deadline).await,
        registry::ToolKind::Uv => install_uv(spec, root, deadline).await,
        registry::ToolKind::Binary | registry::ToolKind::Command | registry::ToolKind::Chrome => {
            return Err(need_setup(format!(
                "{} has no private installer; provide it on PATH",
                spec.name
            )))
        }
    };
    let installed = installed?;
    // Record exactly what this install produced so a later remove can delete
    // only our files and list user additions instead of destroying them.
    write_install_manifest(spec.name, &target, &snapshot)?;

    let scope = if spec.singleton { "global" } else { "private" };
    let relative = if spec.singleton {
        "tools/npm-global".to_string()
    } else {
        format!("tools/{}", spec.name)
    };
    let mut entry = entry_for(
        spec,
        installed.as_deref(),
        scope,
        &relative,
        spec.singleton.then_some("tools/npm-global"),
    );
    if spec.kind == registry::ToolKind::Chrome {
        entry["path"] = json!("tools/chrome");
    }
    record_retry(root, spec.name, entry)
}

async fn ensure_chrome(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
    entry: &Value,
    force: bool,
) -> Result<Value> {
    let owned = entry["owner"] == OURS;
    let Some(binary) = registry::chrome_binary(root) else {
        if force || !owned {
            let (version, digest) = install_chrome(root, deadline).await?;
            let mut entry = entry_for(spec, Some(&version), "private", "tools/chrome", None);
            entry["archive_sha256"] = json!(digest);
            return record_retry(root, spec.name, entry);
        }
        return Err(need_setup(
            "Chrome is not installed; run the browser setup or install Chrome",
        ));
    };
    let private = path_under(&binary, &registry::tool_root(spec.name, root));
    if private {
        if entry["version"].is_string() {
            return Ok(merged_record(spec.name, entry));
        }
        let entry = entry_for(spec, None, "private", "tools/chrome", None);
        return record_retry(root, spec.name, entry);
    }
    if force {
        if owned {
            let (version, digest) = install_chrome(root, deadline).await?;
            let mut entry = entry_for(spec, Some(&version), "private", "tools/chrome", None);
            entry["archive_sha256"] = json!(digest);
            return record_retry(root, spec.name, entry);
        }
        return Err(need_setup(
            "The system Chrome is used as-is; arana does not replace it",
        ));
    }
    Ok(json!({
        "tool": spec.name,
        "owner": "system",
        "scope": "system",
        "version": entry["version"].clone(),
        "kind": "chrome",
        "path": binary.to_string_lossy(),
    }))
}

async fn install_pip(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
    force: bool,
    repair: bool,
) -> Result<Option<String>> {
    let base_python = resolve_python(root)?;
    let tool_dir = registry::tool_root(spec.name, root);
    reach_core::state::private_dir(&tool_dir)?;
    let venv = registry::venv_dir(spec.name, root);
    let venv_python = registry::venv_python(spec.name, root);
    let env = install_env(root)?;

    if !venv_python.is_file() {
        let args = vec![
            "-m".to_string(),
            "venv".to_string(),
            venv.to_string_lossy().into_owned(),
        ];
        run_program(
            &base_python,
            &args,
            root,
            deadline.remaining()?,
            &format!("{}:venv", spec.name),
            &env,
        )
        .await?;
    }
    if !venv_python.is_file() {
        return Err(upstream_broken(format!(
            "the venv for {} has no interpreter",
            spec.name
        )));
    }

    let mut args = vec![
        "-m".to_string(),
        "pip".to_string(),
        "install".to_string(),
        "--quiet".to_string(),
        "--disable-pip-version-check".to_string(),
    ];
    if force {
        args.push("--upgrade".to_string());
    } else if repair {
        // A recorded install whose launcher vanished must be reinstalled, not
        // reported as already satisfied.
        args.push("--force-reinstall".to_string());
    }
    args.push(registry::install_spec(spec));
    run_program(
        &venv_python,
        &args,
        root,
        deadline.remaining()?,
        &format!("{}:pip_packages", spec.name),
        &env,
    )
    .await?;

    Ok(pip_dist_version(spec, root))
}

async fn install_ffmpeg(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
    repair: bool,
) -> Result<Option<String>> {
    let version = install_pip(spec, root, deadline, false, repair).await?;
    let venv_python = registry::venv_python(spec.name, root);
    let args = vec![
        "-c".to_string(),
        "import imageio_ffmpeg; print(imageio_ffmpeg.get_ffmpeg_exe())".to_string(),
    ];
    let stdout = run_program(
        &venv_python,
        &args,
        root,
        deadline.remaining()?,
        "ffmpeg:locate",
        &install_env(root)?,
    )
    .await?;
    let source = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| PathBuf::from(line.trim()))
        .ok_or_else(|| upstream_broken("imageio-ffmpeg did not report an ffmpeg binary"))?;
    if !source.is_file() {
        return Err(upstream_broken(format!(
            "imageio-ffmpeg reported a missing binary: {}",
            source.display()
        )));
    }
    let bin = registry::ffmpeg_bin_dir(root);
    reach_core::state::private_dir(&bin)?;
    let target = bin.join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    reach_core::state::ensure_safe(&target)?;
    if target.exists() {
        std::fs::remove_file(&target).map_err(ReachError::from)?;
    }
    std::fs::copy(&source, &target).map_err(ReachError::from)?;
    Ok(version)
}

async fn install_npm(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
) -> Result<Option<String>> {
    let npm = registry::resolve("npm", root)
        .ok_or_else(|| need_setup("npm is not installed; install Node.js and retry"))?;
    let npm_invocation = registry::invocation_for_resolved("npm", &npm, root);
    let prefix = install_root(spec, root);
    reach_core::state::private_dir(&prefix)?;
    let mut args = vec!["install".to_string()];
    if spec.singleton {
        args.push("-g".to_string());
    }
    args.extend([
        "--prefix".to_string(),
        prefix.to_string_lossy().into_owned(),
        "--no-fund".to_string(),
        "--no-audit".to_string(),
    ]);
    if spec.name == "opencli" {
        // OpenCLI ships its adapters; install hooks must not replace this
        // verified release or write unrelated agent configuration.
        args.push("--ignore-scripts".to_string());
    }
    args.push(registry::install_spec(spec));
    run_invocation(
        &npm_invocation,
        "npm",
        &args,
        root,
        deadline.remaining()?,
        &format!("{}:npm_packages", spec.name),
        &install_env(root)?,
    )
    .await?;

    let command = spec
        .commands
        .first()
        .ok_or_else(|| upstream_broken("the npm tool declares no command"))?;
    if registry::private_bin(spec, command, root).is_none() {
        return Err(upstream_broken(format!(
            "npm finished but the {} launcher is missing",
            spec.name
        )));
    }
    Ok(npm_package_version(&prefix, spec.package))
}

fn platform_arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" | "x86" => Ok("x64"),
        "aarch64" => Ok("arm64"),
        other => Err(need_setup(format!(
            "no supported build for this CPU architecture ({other})"
        ))),
    }
}

async fn fetch_text(url: &str, root: &Path, deadline: &Deadline) -> Result<String> {
    let bytes = ssrf::safe_fetch(url, MAX_METADATA_BYTES, deadline.remaining()?).await?;
    let text = String::from_utf8(bytes)
        .map_err(|_| upstream_broken(format!("{url} did not return UTF-8 text")))?;
    let _ = root;
    Ok(text)
}

/// Node.js release metadata: the index.json `files` key, the archive stem
/// platform and the archive extension. Windows publishes a zip; Linux and
/// macOS publish the official tar.gz (macOS archive names use `darwin` while
/// the index key is `osx-<arch>-tar`).
fn node_release_target(arch: &str) -> Result<(String, String, &'static str)> {
    node_release_target_for(std::env::consts::OS, arch)
}

fn node_release_target_for(os: &str, arch: &str) -> Result<(String, String, &'static str)> {
    match os {
        "windows" => Ok((format!("win-{arch}-zip"), format!("win-{arch}"), "zip")),
        "macos" => Ok((
            format!("osx-{arch}-tar"),
            format!("darwin-{arch}"),
            "tar.gz",
        )),
        "linux" => Ok((format!("linux-{arch}"), format!("linux-{arch}"), "tar.gz")),
        other => Err(need_setup(format!(
            "no Node.js build for this platform ({other})"
        ))),
    }
}

/// Installed release root for a Node archive: on Windows `node.exe` sits at the
/// root; on Unix `bin/node` sits one level below `lib/node_modules` and the
/// supporting tree (`lib`, `include`, `share`) must be installed together.
fn node_release_root(extracted: &[PathBuf]) -> Option<PathBuf> {
    node_release_root_in(extracted, cfg!(windows))
}

fn node_release_root_in(extracted: &[PathBuf], windows: bool) -> Option<PathBuf> {
    let exe = if windows { "node.exe" } else { "node" };
    let executable = extracted.iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(exe))
    })?;
    let parent = executable.parent()?;
    if !windows && parent.file_name().is_some_and(|name| name == "bin") {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

async fn install_node(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
) -> Result<Option<String>> {
    let arch = platform_arch()?;
    let (key, platform, extension) = node_release_target(arch)?;
    let index = fetch_text("https://nodejs.org/dist/index.json", root, deadline).await?;
    let releases: Value = serde_json::from_str(&index)
        .map_err(|error| upstream_broken(format!("nodejs.org index is not JSON: {error}")))?;
    let release = releases
        .as_array()
        .and_then(|releases| {
            releases.iter().find(|release| {
                release["lts"].as_str().is_some_and(|lts| !lts.is_empty())
                    && release["files"]
                        .as_array()
                        .is_some_and(|files| files.iter().any(|file| file == &json!(key)))
            })
        })
        .ok_or_else(|| upstream_broken("no Node.js LTS build for this machine"))?;
    let version = release["version"]
        .as_str()
        .ok_or_else(|| upstream_broken("the Node.js release has no version"))?
        .to_string();
    let stem = format!("node-{version}-{platform}");
    let filename = format!("{stem}.{extension}");
    let base = format!("https://nodejs.org/dist/{version}");
    let sums = fetch_text(&format!("{base}/SHASUMS256.txt"), root, deadline).await?;
    let sha = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            let name = parts.next()?;
            (name.trim_start_matches('*') == filename && digest.len() == 64)
                .then(|| digest.to_string())
        })
        .ok_or_else(|| upstream_broken(format!("SHASUMS256.txt has no entry for {filename}")))?;
    let archive = download::download_file(
        &format!("{base}/{filename}"),
        root,
        &sha,
        MAX_ARCHIVE_BYTES,
        deadline.remaining()?,
    )
    .await?;
    let tmp = root.join("tmp");
    reach_core::state::private_dir(&tmp)?;
    let staging = tempfile::Builder::new()
        .prefix("node-install-")
        .tempdir_in(&tmp)
        .map_err(ReachError::from)?;
    // The official Unix tarball carries legit symlink shims (`bin/npm`,
    // `bin/npx`); the regular-member extractor keeps every file and never
    // creates a link. The Windows zip stays on the strict extractor.
    let extracted = if extension == "zip" {
        download::extract_archive(&archive, staging.path()).await?
    } else {
        download::extract_archive_regular_members(&archive, staging.path()).await?
    };
    let source = node_release_root(&extracted)
        .ok_or_else(|| upstream_broken("the Node.js archive has no node executable"))?;
    install_tree(spec, root, &source)?;
    if !private_ok(spec, root) {
        return Err(upstream_broken(
            "the Node.js release is missing node/npm/npx after installation",
        ));
    }
    let installed = registry::resolve("node", root);
    let detected = match installed {
        Some(path) => probe_version(&path, spec, root, deadline.remaining()?).await,
        None => None,
    };
    Ok(detected.or_else(|| Some(version.trim_start_matches('v').to_string())))
}

async fn install_uv(
    spec: &registry::ToolSpec,
    root: &Path,
    deadline: &Deadline,
) -> Result<Option<String>> {
    let arch = platform_arch()?;
    let (target, ext) = match std::env::consts::OS {
        "windows" => (format!("{arch}-pc-windows-msvc"), "zip"),
        "macos" => (format!("{arch}-apple-darwin"), "tar.gz"),
        "linux" => (format!("{arch}-unknown-linux-gnu"), "tar.gz"),
        other => {
            return Err(need_setup(format!(
                "no uv build for this platform ({other})"
            )))
        }
    };
    let api = fetch_text(
        "https://api.github.com/repos/astral-sh/uv/releases/latest",
        root,
        deadline,
    )
    .await?;
    let release: Value = serde_json::from_str(&api)
        .map_err(|error| upstream_broken(format!("the uv release API is not JSON: {error}")))?;
    let name = format!("uv-{target}.{ext}");
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| upstream_broken("the uv release has no assets"))?;
    let asset = assets
        .iter()
        .find(|asset| asset["name"].as_str() == Some(name.as_str()))
        .ok_or_else(|| upstream_broken(format!("the uv release has no {name} asset")))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| upstream_broken("the uv asset has no download URL"))?;
    let mut sha = asset["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_string);
    if sha.is_none() {
        let (sidecar, _) =
            download::download_cached(&format!("{url}.sha256"), root, 4096, deadline.remaining()?)
                .await?;
        let text = std::fs::read_to_string(&sidecar).map_err(ReachError::from)?;
        sha = text
            .split_whitespace()
            .find(|token| token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string);
    }
    let sha = sha.ok_or_else(|| upstream_broken("the uv release publishes no sha256"))?;
    let archive =
        download::download_file(url, root, &sha, MAX_ARCHIVE_BYTES, deadline.remaining()?).await?;
    let tmp = root.join("tmp");
    reach_core::state::private_dir(&tmp)?;
    let staging = tempfile::Builder::new()
        .prefix("uv-install-")
        .tempdir_in(&tmp)
        .map_err(ReachError::from)?;
    let extracted = download::extract_archive(&archive, staging.path()).await?;
    let exe_name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let uv = extracted
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(exe_name))
        })
        .ok_or_else(|| upstream_broken("the uv archive has no uv executable"))?;
    let source = uv
        .parent()
        .ok_or_else(|| upstream_broken("the uv executable has no parent directory"))?;
    install_tree(spec, root, source)?;
    let installed = registry::resolve("uv", root);
    let detected = match installed {
        Some(path) => probe_version(&path, spec, root, deadline.remaining()?).await,
        None => None,
    };
    Ok(detected.or_else(|| {
        release["tag_name"]
            .as_str()
            .map(|tag| tag.trim_start_matches('v').to_string())
    }))
}

/// Move an extracted release tree into the tool's canonical directory.
fn install_tree(spec: &registry::ToolSpec, root: &Path, source: &Path) -> Result<()> {
    let destination = match spec.kind {
        registry::ToolKind::Node => registry::node_current_dir(root),
        registry::ToolKind::Uv => registry::uv_bin_dir(root),
        registry::ToolKind::Chrome => registry::chrome_current_dir(root),
        _ => {
            return Err(bad_input(format!(
                "{} is not installed from a release tree",
                spec.name
            )))
        }
    };
    if destination.exists() {
        // With a manifest only our recorded files are cleared, so a repair
        // never destroys files someone else placed in the owned tree.
        match read_install_manifest(&destination) {
            Some(manifest) if manifest["truncated"] != true => {
                let _ = remove_owned_files(&destination, &manifest);
                if !dir_is_non_empty(&destination) {
                    let _ = std::fs::remove_dir(&destination);
                }
            }
            _ => {
                fsguard::remove_tree_guarded(&destination)?;
            }
        }
    }
    reach_core::state::private_dir(
        destination
            .parent()
            .ok_or_else(|| bad_input("install destination has no parent"))?,
    )?;
    if std::fs::rename(source, &destination).is_err() {
        copy_tree(source, &destination)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    reach_core::state::private_dir(destination)?;
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        for entry in std::fs::read_dir(&from)
            .map_err(ReachError::from)?
            .flatten()
        {
            let path = entry.path();
            let target = to.join(entry.file_name());
            let meta = std::fs::symlink_metadata(&path).map_err(ReachError::from)?;
            if fsguard::is_link_like(&meta) {
                return Err(upstream_broken(format!(
                    "the release archive contains a link: {}",
                    path.display()
                )));
            }
            if meta.is_dir() {
                reach_core::state::private_dir(&target)?;
                stack.push((path, target));
            } else if meta.is_file() {
                std::fs::copy(&path, &target).map_err(ReachError::from)?;
            }
        }
    }
    Ok(())
}

/// Chrome for Testing, stable channel. The zip carries no published checksum,
/// so the received digest is recorded in the ledger and the extractor still
/// applies its traversal/link/size guards.
async fn install_chrome(root: &Path, deadline: &Deadline) -> Result<(String, String)> {
    let json_text = fetch_text(
        "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json",
        root,
        deadline,
    )
    .await?;
    let manifest: Value = serde_json::from_str(&json_text).map_err(|error| {
        upstream_broken(format!(
            "the Chrome for Testing manifest is not JSON: {error}"
        ))
    })?;
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => "win64",
        ("linux", "x86_64") => "linux64",
        ("macos", "x86_64") => "mac-x64",
        ("macos", "aarch64") => "mac-arm64",
        (os, arch) => {
            return Err(need_setup(format!(
                "Chrome for Testing has no build for {os} {arch}"
            )))
        }
    };
    let stable = &manifest["channels"]["Stable"];
    let version = stable["version"]
        .as_str()
        .ok_or_else(|| upstream_broken("the Chrome manifest has no Stable version"))?;
    let downloads = stable["downloads"]["chrome"]
        .as_array()
        .ok_or_else(|| upstream_broken("the Chrome manifest has no chrome downloads"))?;
    let url = downloads
        .iter()
        .find(|entry| entry["platform"].as_str() == Some(platform))
        .and_then(|entry| entry["url"].as_str())
        .ok_or_else(|| upstream_broken("the Chrome manifest has no build for this platform"))?;
    let (archive, digest) =
        download::download_cached(url, root, MAX_ARCHIVE_BYTES, deadline.remaining()?).await?;
    let tmp = root.join("tmp");
    reach_core::state::private_dir(&tmp)?;
    let staging = tempfile::Builder::new()
        .prefix("chrome-install-")
        .tempdir_in(&tmp)
        .map_err(ReachError::from)?;
    let extracted = download::extract_archive(&archive, staging.path()).await?;
    let exe_name = if cfg!(windows) {
        "chrome.exe"
    } else {
        "chrome"
    };
    let exe = extracted
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(exe_name))
        })
        .ok_or_else(|| upstream_broken("the Chrome archive has no chrome executable"))?;
    let source = exe
        .parent()
        .ok_or_else(|| upstream_broken("the Chrome executable has no parent directory"))?;
    let chrome_spec = spec("chrome")?;
    install_tree(chrome_spec, root, source)?;
    Ok((version.to_string(), digest))
}

/// Inspect one tool. Missing tools are `installed: false` (never an error) so
/// `doctor` can report every channel/tool in one pass.
pub async fn inspect_tool(name: &str, root: &Path) -> Result<Value> {
    let spec = spec(name)?;
    let store = reach_core::state::Store::new(root);
    let entry = store.load()?["tools"][name].clone();
    let mut commands = Vec::new();
    for command in spec.commands {
        if let Some(path) = registry::resolve(command, root) {
            let source = if path_under(&path, &registry::tool_root(spec.name, root))
                || (spec.singleton && path_under(&path, &registry::npm_global_dir(root)))
            {
                "private"
            } else {
                "system"
            };
            commands.push(json!({
                "command": command,
                "path": path.to_string_lossy(),
                "source": source,
            }));
        }
    }
    let installed = present(spec, root);
    let version = if installed {
        resolved_version(spec, root, Duration::from_secs(20)).await
    } else {
        None
    };
    let version_value = version
        .clone()
        .or_else(|| entry["version"].as_str().map(str::to_string));
    let has_limits = spec.floor.is_some() || !spec.exclude.is_empty();
    let allowed = if has_limits {
        version_value
            .as_deref()
            .map(|value| registry::allowed(spec, value))
            .unwrap_or(false)
    } else {
        true
    };
    let owner = if entry.is_object() {
        entry["owner"].clone()
    } else if installed {
        json!("system")
    } else {
        Value::Null
    };
    let scope = if entry.is_object() {
        entry["scope"].clone()
    } else if installed {
        json!("system")
    } else {
        Value::Null
    };
    let mut out = json!({
        "tool": spec.name,
        "kind": spec.kind.as_str(),
        "installed": installed,
        "owner": owner,
        "scope": scope,
        "version": version_value,
        "tested": spec.tested,
        "floor": spec.floor,
        "exclude": spec.exclude,
        "allowed": allowed,
        "singleton": spec.singleton,
        "needs": spec.needs,
        "commands": commands,
    });
    if !installed {
        out["reason"] = json!("not installed");
        out["hint"] = json!(format!(
            "run `arana add <channel>` to install {}",
            spec.name
        ));
    }
    if let Some(path) = entry["path"].as_str() {
        out["path"] = json!(path);
    }
    if let Some(prefix) = entry["prefix"].as_str() {
        out["prefix"] = json!(prefix);
    }
    Ok(out)
}

/// Refresh an arana-owned tool to its tested version (or the newest version
/// inside floor/exclude when no tested pin exists). Never modifies a user or
/// system copy: those are reported instead of upgraded.
pub async fn update_tool(name: &str, root: &Path, timeout: Duration) -> Result<Value> {
    let spec = spec(name)?;
    if spec.kind == registry::ToolKind::Command {
        return Err(need_setup(format!(
            "{} is an external command that arana does not manage",
            spec.name
        )));
    }
    if spec.name == "obscura" {
        return obscura::install_obscura(root).await;
    }
    let deadline = Deadline::new(timeout);
    let _lock =
        reach_core::state::FileLock::wait(root, &format!("tool-install-{name}"), timeout).await?;
    let store = reach_core::state::Store::new(root);
    let entry = store.load()?["tools"][name].clone();
    let owned = entry["owner"] == OURS;
    if entry.is_object() && !owned {
        return Err(need_setup(format!(
            "{} is {}'s copy; arana never modifies a user or system install",
            spec.name, entry["owner"]
        )));
    }
    // A system yt-dlp inside the limits is reported as-is, never upgraded.
    if pip_reuses_system(spec) && !owned && !private_ok(spec, root) && system_present(spec, root) {
        if let Some(current) = resolved_version(spec, root, deadline.remaining()?).await {
            if registry::allowed(spec, &current) {
                return Ok(json!({
                    "tool": spec.name,
                    "updated": false,
                    "owner": "system",
                    "version": current,
                    "reason": "the system copy is used as-is",
                }));
            }
        }
    }
    if matches!(
        spec.kind,
        registry::ToolKind::Node
            | registry::ToolKind::Uv
            | registry::ToolKind::Ffmpeg
            | registry::ToolKind::Chrome
    ) && !owned
        && present(spec, root)
    {
        return Ok(json!({
            "tool": spec.name,
            "updated": false,
            "owner": "system",
            "reason": "the system copy is used as-is",
        }));
    }
    for dependency in spec.needs {
        let remaining = deadline.remaining()?;
        Box::pin(ensure_tool(dependency, root, remaining)).await?;
    }
    let mut updated = ensure_locked(spec, root, &deadline, true).await?;
    updated["updated"] = json!(true);
    Ok(updated)
}

/// Files remaining below a directory (links are reported, never traversed).
fn list_files(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    if !fsguard::is_real_dir(dir) {
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() && !fsguard::is_link_like(&meta) {
                stack.push(path);
            } else {
                out.push(Value::String(path.to_string_lossy().into_owned()));
            }
        }
    }
    out.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    out
}

/// Per-install manifest sidecar: the regular files and links arana's
/// installer produced. Removal uses it to delete only what we created and to
/// report files someone else later dropped into the owned tree.
const MANIFEST_NAME: &str = ".arana-manifest.json";
const MANIFEST_MAX_FILES: usize = 25_000;

fn manifest_path(install: &Path) -> PathBuf {
    install.join(MANIFEST_NAME)
}

/// Every non-directory entry below `install` (links included, link-like
/// directories never traversed), as `/`-separated relative paths.
fn walk_relative(install: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if !fsguard::is_real_dir(install) {
        return out;
    }
    let mut stack = vec![install.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() && !fsguard::is_link_like(&meta) {
                stack.push(path);
            } else if entry.file_name().to_str() != Some(MANIFEST_NAME) {
                out.push(
                    path.strip_prefix(install)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out.sort();
    out
}

#[derive(Default)]
struct InstallSnapshot {
    /// Entries present before this install ran.
    pre: std::collections::HashSet<String>,
    /// Entries the previous manifest already claimed as ours.
    base: std::collections::HashSet<String>,
}

fn snapshot_install(install: &Path) -> InstallSnapshot {
    let base = read_install_manifest(install)
        .and_then(|manifest| {
            manifest["files"].as_array().map(|files| {
                files
                    .iter()
                    .filter_map(|file| file.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default();
    InstallSnapshot {
        pre: walk_relative(install).into_iter().collect(),
        base,
    }
}

/// Write the manifest after a completed install. Files that were present
/// before the install and were not already ours stay unowned, so a later
/// removal preserves them. Oversized trees record a truncated marker and
/// removal falls back to wholesale.
fn write_install_manifest(name: &str, install: &Path, snapshot: &InstallSnapshot) -> Result<()> {
    let user_added: std::collections::HashSet<&String> = snapshot
        .pre
        .iter()
        .filter(|path| !snapshot.base.contains(*path))
        .collect();
    let files: Vec<String> = walk_relative(install)
        .into_iter()
        .filter(|path| !user_added.contains(path))
        .collect();
    let body = if files.len() > MANIFEST_MAX_FILES {
        json!({"tool": name, "truncated": true})
    } else {
        json!({"tool": name, "files": files})
    };
    reach_core::state::atomic_write(&manifest_path(install), body.to_string().as_bytes())
}

fn read_install_manifest(install: &Path) -> Option<Value> {
    let text = reach_core::state::read_private(&manifest_path(install), 8 * 1024 * 1024).ok()?;
    serde_json::from_str(&text).ok()
}

/// Delete exactly the manifest-recorded files (a link deletion removes the
/// link, never its target), then the manifest itself, pruning directories
/// that became empty. Returns `(deleted, skipped)`.
fn remove_owned_files(canonical: &Path, manifest: &Value) -> (usize, Vec<Value>) {
    let mut deleted = 0usize;
    let mut skipped = Vec::new();
    for entry in manifest["files"].as_array().cloned().unwrap_or_default() {
        let Some(relative) = entry.as_str().map(str::to_owned) else {
            skipped.push(entry);
            continue;
        };
        let candidate = Path::new(&relative);
        if relative.is_empty()
            || candidate.is_absolute()
            || candidate
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            skipped.push(json!(relative));
            continue;
        }
        let path = canonical.join(candidate);
        // Validate ancestors only: a recorded leaf may legitimately be the
        // venv interpreter link, and `remove_file` never touches its target.
        let ancestors_safe = path
            .parent()
            .is_some_and(|parent| reach_core::state::ensure_safe(parent).is_ok());
        if !ancestors_safe {
            skipped.push(json!(relative));
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
                if std::fs::remove_file(&path).is_ok() {
                    deleted += 1;
                } else {
                    skipped.push(json!(relative));
                }
            }
            Ok(_) => skipped.push(json!(relative)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => skipped.push(json!(relative)),
        }
    }
    let _ = std::fs::remove_file(manifest_path(canonical));
    prune_empty_dirs(canonical);
    (deleted, skipped)
}

fn prune_empty_dirs(root: &Path) {
    let mut dirs = Vec::new();
    let mut walk = vec![root.to_path_buf()];
    while let Some(dir) = walk.pop() {
        if !fsguard::is_real_dir(&dir) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if fsguard::is_real_dir(&path) {
                walk.push(path.clone());
                dirs.push(path);
            }
        }
    }
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in dirs {
        if !dir_is_non_empty(&dir) {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

/// Remove one arana-owned private install and its ledger record.
///
/// Meaning of remove: delete only files the install manifest records (and the
/// owned directory when nothing else is left), after verifying the ledger
/// says `owner: arana` and that no component of the canonical path is a
/// link. Files someone else added inside the owned tree are listed in
/// `left_untouched`, never deleted. Without a manifest (legacy or oversized
/// trees) the whole owned directory is removed instead. A user/system copy is
/// never touched, and forged ledger paths are ignored because the path is
/// recomputed from the registry, not read from state.
pub fn remove_tool(name: &str, root: &Path) -> Result<Value> {
    let spec = spec(name)?;
    if spec.name == "obscura" {
        return obscura::remove_obscura(root);
    }
    let _lock = reach_core::state::FileLock::try_acquire(root, &format!("tool-install-{name}"))?;
    let store = reach_core::state::Store::new(root);
    let entry = store.load()?["tools"][name].clone();
    if !entry.is_object() {
        return Ok(json!({
            "tool": spec.name,
            "removed": false,
            "reason": "not installed",
        }));
    }
    if entry["owner"] != OURS {
        let canonical = install_root(spec, root);
        return Ok(json!({
            "tool": spec.name,
            "removed": false,
            "reason": format!("owner is {}", entry["owner"]),
            "left_untouched": list_files(&canonical),
        }));
    }

    // Never trust the ledger's path string; recompute the canonical tree.
    let canonical = install_root(spec, root);
    if canonical.exists() {
        match std::fs::symlink_metadata(&canonical) {
            Ok(meta) if !meta.is_dir() || fsguard::is_link_like(&meta) => {
                return Ok(json!({
                    "tool": spec.name,
                    "removed": false,
                    "reason": "the private tool path is a link; left untouched",
                }));
            }
            Ok(_) => {}
            Err(error) => return Err(ReachError::from(error)),
        }
        reach_core::state::ensure_safe(&canonical)?;
    }
    if let Some(manifest) = read_install_manifest(&canonical) {
        if manifest["truncated"] != true {
            let (deleted, skipped) = remove_owned_files(&canonical, &manifest);
            drop_record(root, spec.name)?;
            if !dir_is_non_empty(&canonical) {
                let _ = std::fs::remove_dir(&canonical);
            }
            return Ok(json!({
                "tool": spec.name,
                "removed": true,
                "deleted": deleted,
                "skipped_entries": skipped,
                "left_untouched": list_files(&canonical),
            }));
        }
    }
    let (deleted, skipped) = fsguard::remove_tree_guarded(&canonical)?;
    drop_record(root, spec.name)?;
    let left = list_files(&canonical);
    Ok(json!({
        "tool": spec.name,
        "removed": true,
        "deleted": deleted,
        "skipped_entries": skipped,
        "left_untouched": left,
    }))
}

#[cfg(test)]
mod tests {
    use super::{node_release_root_in, node_release_target_for};
    use std::path::PathBuf;

    fn paths(strings: &[&str]) -> Vec<PathBuf> {
        strings.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn node_release_metadata_matches_official_index_and_archive_names() {
        let linux = node_release_target_for("linux", "x64").expect("linux");
        assert_eq!(
            linux,
            ("linux-x64".to_string(), "linux-x64".to_string(), "tar.gz")
        );
        let mac = node_release_target_for("macos", "arm64").expect("macos");
        assert_eq!(
            mac,
            (
                "osx-arm64-tar".to_string(),
                "darwin-arm64".to_string(),
                "tar.gz"
            )
        );
        let win = node_release_target_for("windows", "x64").expect("windows");
        assert_eq!(
            win,
            ("win-x64-zip".to_string(), "win-x64".to_string(), "zip")
        );
        assert!(node_release_target_for("freebsd", "x64").is_err());
    }

    #[test]
    fn node_release_root_keeps_the_unix_lib_tree_and_windows_flat_root() {
        let unix = paths(&[
            "node-v24.14.1-linux-x64/bin/node",
            "node-v24.14.1-linux-x64/lib/node_modules/npm/bin/npm-cli.js",
        ]);
        assert_eq!(
            node_release_root_in(&unix, false),
            Some(PathBuf::from("node-v24.14.1-linux-x64")),
            "Unix installs the archive root so lib/node_modules survives"
        );
        let win = paths(&[
            "node-v24.14.1-win-x64/node.exe",
            "node-v24.14.1-win-x64/npm.cmd",
        ]);
        assert_eq!(
            node_release_root_in(&win, true),
            Some(PathBuf::from("node-v24.14.1-win-x64"))
        );
    }
}
