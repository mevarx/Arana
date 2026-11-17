//! The obscura web backend: pinned-binary install, `fetch --dump text` reads
//! and owned-only removal.
//!
//! obscura is consumed as a pinned release binary under
//! `state_dir/tools/obscura/bin/` — never vendored, never on the user's PATH.
//! The Windows registry entry pulls the `-stealth` archive; its
//! absence would degrade to the non-stealth build, never to failure. The
//! install verifies the pinned sha256, extracts with zip-slip/symlink/size
//! refusal and keeps the bundled runtime DLLs beside both executables. The full
//! Apache-2.0 license (audited revision) and, when the upstream archive has no
//! NOTICE, a local attribution file are installed and ledgered with the
//! payload. `remove` deletes only regular files recorded with `owner: arana`
//! inside the tool's own `bin/` subtree, never through a link.

use crate::download::{download_file, extract_archive};
use crate::fsguard;
use crate::registry;
use crate::runner::{self, CommandSpec};
use crate::ssrf;
use reach_core::{make_item, ReachError, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Web-read response cap (upstream `_MAX_RESPONSE_BYTES`).
const MAX_READ_BYTES: usize = 5 * 1024 * 1024;
/// Archive download cap: the Windows stealth zip is ~74 MB.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// Serializes install/remove (staging, ledger repair and owned-file removal).
const OBSCURA_LOCK: &str = "reach-tools-obscura";
/// Full Apache-2.0 text copied from the audited obscura revision (0d9a2f7).
const OBSCURA_LICENSE: &str = include_str!("../../../THIRD_PARTY/obscura/LICENSE");

/// The pinned release for this platform.
#[derive(Clone, Debug)]
pub struct ObscuraRelease {
    pub version: &'static str,
    pub asset: &'static str,
    pub sha256: &'static str,
}

/// Pinned obscura release, kind `binary`, with `tested` and `floor`:
/// v0.2.7 of h4ckf0r0day/obscura, Windows pulls the stealth archive.
/// Checksums are the ones published on the v0.2.7 release page.
pub fn pinned_release() -> Result<ObscuraRelease> {
    let release = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ObscuraRelease {
            version: "0.2.7",
            asset: "obscura-x86_64-windows-stealth.zip",
            sha256: "ffae5e71cfca0108709b57ad83ce7d6b588eff6a15a43093b0cf867dbd18c867",
        },
        ("linux", "x86_64") => ObscuraRelease {
            version: "0.2.7",
            asset: "obscura-x86_64-linux-stealth.tar.gz",
            sha256: "1d6514beacef9fe75671f4bf7a66d3a7bceef9bde39f8156d1ca4a701eb1e216",
        },
        ("linux", "aarch64") => ObscuraRelease {
            version: "0.2.7",
            asset: "obscura-aarch64-linux-stealth.tar.gz",
            sha256: "f51167d7ae98be6c6327d3e7e015e74430e190ea6846589e3c131e10641a52b7",
        },
        ("macos", "x86_64") => ObscuraRelease {
            version: "0.2.7",
            asset: "obscura-x86_64-macos-stealth.tar.gz",
            sha256: "62bd094388a1f97480fd06be349299e721a3488c216c86a2fce9367310594ec3",
        },
        ("macos", "aarch64") => ObscuraRelease {
            version: "0.2.7",
            asset: "obscura-aarch64-macos-stealth.tar.gz",
            sha256: "3b05ebcd4755a325be224992fad6a038d86f7e5ab483e4c781f14de06cab973f",
        },
        (os, arch) => {
            return Err(ReachError::new(
                "NEED_SETUP",
                format!("no obscura release for {os} {arch}"),
            ))
        }
    };
    Ok(release)
}

/// Registry base URL (override for mirrors/tests).
fn registry_base() -> String {
    std::env::var("ARANA_TOOL_REGISTRY").unwrap_or_else(|_| "https://github.com".into())
}

/// The pinned download URL for this platform's release.
fn release_url(release: &ObscuraRelease) -> String {
    format!(
        "{}/h4ckf0r0day/obscura/releases/download/v{}/{}",
        registry_base(),
        release.version,
        release.asset
    )
}

/// The pinned sha256 (override for mirrors/tests).
fn pinned_sha256(release: &ObscuraRelease) -> String {
    std::env::var("ARANA_OBSCURA_SHA256").unwrap_or_else(|_| release.sha256.to_string())
}

fn exe_name(command: &str) -> String {
    if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

fn tool_paths(state_dir: &Path) -> (PathBuf, PathBuf) {
    let tool_dir = state_dir.join("tools").join("obscura");
    let bin_dir = tool_dir.join("bin");
    (tool_dir, bin_dir)
}

/// Does the directory exist and hold anything?
fn dir_is_non_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// A ledger path is owned only when it is a normal relative path under
/// `tools/obscura/bin/`. Returns the path relative to `bin/`.
fn owned_bin_relative(recorded: &str) -> Option<PathBuf> {
    let path = Path::new(recorded);
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(part)) if part == "tools" => {}
        _ => return None,
    }
    match components.next() {
        Some(Component::Normal(part)) if part == "obscura" => {}
        _ => return None,
    }
    match components.next() {
        Some(Component::Normal(part)) if part == "bin" => {}
        _ => return None,
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

fn ledger_key(relative: &Path) -> String {
    relative.to_string_lossy().replace('\\', "/")
}

fn recorded_bin_paths(existing: &Value) -> HashSet<String> {
    existing["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .filter_map(owned_bin_relative)
                .map(|relative| ledger_key(&relative))
                .collect()
        })
        .unwrap_or_default()
}

/// The ledger record describes a complete, current install: arana-owned,
/// pinned version, every recorded file present as a real file, and the main
/// executable included.
fn installed_record_is_current(existing: &Value, bin_dir: &Path, release: &ObscuraRelease) -> bool {
    if existing["owner"] != "arana" || existing["version"] != release.version {
        return false;
    }
    let Some(files) = existing["files"].as_array() else {
        return false;
    };
    if files.is_empty() {
        return false;
    }
    let exe = exe_name("obscura");
    let mut has_exe = false;
    let mut has_license = false;
    let mut has_notice = false;
    for file in files {
        let Some(relative) = file.as_str().and_then(owned_bin_relative) else {
            return false;
        };
        let path = bin_dir.join(&relative);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && !fsguard::is_link_like(&meta) => {}
            _ => return false,
        }
        if relative == Path::new(&exe) {
            has_exe = true;
        } else if relative == Path::new("LICENSE") {
            has_license = true;
        } else if relative == Path::new("NOTICE") {
            has_notice = true;
        }
    }
    has_exe && has_license && has_notice
}

/// Local attribution installed beside the binaries when the upstream archive
/// carries no NOTICE (the audited NOTICE URL returned 404).
fn attribution(release: &ObscuraRelease) -> String {
    format!(
        "obscura {} — https://github.com/h4ckf0r0day/obscura\n\
         Copyright (c) obscura contributors\n\
         Licensed under the Apache License, Version 2.0.\n\
         A copy of the license is installed alongside this file as LICENSE.\n",
        release.version
    )
}

fn record_file(recorded: &mut Vec<Value>, seen: &mut HashSet<String>, relative: &Path) {
    let key = ledger_key(relative);
    if seen.insert(key.clone()) {
        recorded.push(Value::String(format!("tools/obscura/bin/{key}")));
    }
}

/// Install the pinned obscura release under `state_dir/tools/obscura/`,
/// verifying the registry sha256, extracting safely (DLLs kept), installing
/// the audited license plus local attribution when the archive has no NOTICE,
/// and recording owner/scope/version/files in `state.json`. Idempotent only
/// when every recorded file (and the pinned version) is intact.
pub async fn install_obscura(state_dir: &Path) -> Result<Value> {
    let release = pinned_release()?;
    let (tool_dir, bin_dir) = tool_paths(state_dir);
    // One tool-level lock covers staging, repair and removal.
    let _lock = reach_core::state::FileLock::try_acquire(state_dir, OBSCURA_LOCK)?;
    let store = reach_core::state::Store::new(state_dir);
    let existing = store.load()?["tools"]["obscura"].clone();

    // Idempotent: ours, complete, same version → return the record.
    if installed_record_is_current(&existing, &bin_dir, &release) {
        return Ok(merged_record("obscura", existing));
    }

    // Refuse to touch files we did not install (upstream `_install` guard).
    if tool_dir.exists() && dir_is_non_empty(&tool_dir) && existing["owner"] != "arana" {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Unrecorded files occupy the obscura install directory; left untouched",
        ));
    }

    let url = release_url(&release);
    let sha256 = pinned_sha256(&release);
    let archive =
        download_file(&url, state_dir, &sha256, MAX_ARCHIVE_BYTES, INSTALL_TIMEOUT).await?;

    // Stage under a guarded tmp root in a fresh, uniquely named directory, so
    // no predictable path can be cleared through a junction.
    let tmp_root = state_dir.join("tmp");
    reach_core::state::private_dir(&tmp_root)?;
    reach_core::state::ensure_safe(&tmp_root)?;
    let staging = tempfile::Builder::new()
        .prefix("obscura-install-")
        .tempdir_in(&tmp_root)
        .map_err(ReachError::from)?;
    let extracted = extract_archive(&archive, staging.path()).await?;

    // Find the directory holding the main executable (archives may nest).
    let exe = exe_name("obscura");
    let main_exe = extracted
        .iter()
        .find(|f| f.file_name().map(|n| n == exe.as_str()).unwrap_or(false))
        .ok_or_else(|| {
            ReachError::new(
                "UPSTREAM_BROKEN",
                "the obscura archive does not contain the obscura executable",
            )
        })?;
    let src_dir = main_exe.parent().unwrap().to_path_buf();

    let archive_files: Vec<(PathBuf, PathBuf)> = extracted
        .iter()
        .filter_map(|file| {
            file.strip_prefix(&src_dir)
                .ok()
                .map(|relative| (relative.to_path_buf(), file.clone()))
        })
        .collect();
    let archive_notice = archive_files
        .iter()
        .any(|(relative, _)| relative == Path::new("NOTICE"));
    let mut supplemental = vec![PathBuf::from("LICENSE")];
    if !archive_notice {
        supplemental.push(PathBuf::from("NOTICE"));
    }

    // Never overwrite an existing file unless the current ledger records it as
    // ours (repair may replace recorded files, nothing else).
    let previous = recorded_bin_paths(&existing);
    for relative in archive_files
        .iter()
        .map(|(relative, _)| relative)
        .chain(supplemental.iter())
    {
        let target = bin_dir.join(relative);
        reach_core::state::ensure_safe(&target)?;
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if !previous.contains(&ledger_key(relative))
                || !meta.is_file()
                || fsguard::is_link_like(&meta)
            {
                return Err(ReachError::new(
                    "NEED_SETUP",
                    "Unrecorded files occupy the obscura install directory; left untouched",
                ));
            }
        }
    }

    // Commit: move every extracted file under src_dir into the private bin
    // dir (same volume → rename), replacing only recorded files.
    reach_core::state::private_dir(&bin_dir)?;
    reach_core::state::ensure_safe(&bin_dir)?;
    let mut recorded: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (relative, file) in &archive_files {
        if let Some(parent) = relative.parent() {
            if !parent.as_os_str().is_empty() {
                fsguard::create_dir_chain(&bin_dir, parent)?;
            }
        }
        let target = bin_dir.join(relative);
        reach_core::state::ensure_safe(&target)?;
        if std::fs::symlink_metadata(&target).is_ok() {
            std::fs::remove_file(&target).map_err(ReachError::from)?;
        }
        match std::fs::rename(file, &target) {
            Ok(()) => {}
            Err(_) => {
                std::fs::copy(file, &target).map_err(ReachError::from)?;
                let _ = std::fs::remove_file(file);
            }
        }
        record_file(&mut recorded, &mut seen, relative);
    }

    // Ship the full audited license with the installed tool, plus local
    // attribution when the archive has no NOTICE of its own.
    reach_core::state::atomic_write(&bin_dir.join("LICENSE"), OBSCURA_LICENSE.as_bytes())?;
    record_file(&mut recorded, &mut seen, Path::new("LICENSE"));
    if !archive_notice {
        let notice = attribution(&release);
        reach_core::state::atomic_write(&bin_dir.join("NOTICE"), notice.as_bytes())?;
        record_file(&mut recorded, &mut seen, Path::new("NOTICE"));
    }

    if recorded.is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "the obscura archive contained no files to install",
        ));
    }

    let entry = json!({
        "owner": "arana",
        "scope": "private",
        "version": release.version,
        "path": "tools/obscura/bin",
        "archive": release.asset,
        "files": recorded,
    });
    registry::record(state_dir, "obscura", entry)
}

fn merged_record(name: &str, entry: Value) -> Value {
    let mut out = json!({"tool": name});
    if let (Some(dst), Some(src)) = (out.as_object_mut(), entry.as_object()) {
        for (key, value) in src {
            dst.insert(key.clone(), value.clone());
        }
    }
    out
}

/// Read one public URL through `obscura fetch --dump text --quiet` and return
/// the unified item shape. The URL must pass the SSRF guard;
/// `--allow-private-network` is never passed. Empty output is a failure so
/// dispatch can fall through to the next backend.
pub async fn obscura_read(url: &str, timeout: Duration, state_dir: &Path) -> Result<Vec<Value>> {
    let url = ssrf::normalize_public_http_url(url)
        .map_err(|_| ReachError::new("BAD_INPUT", "target must be a public http(s) URL"))?;
    let exe = registry::resolve("obscura", state_dir)
        .ok_or_else(|| ReachError::new("NEED_SETUP", "obscura is not installed"))?;
    let seconds = timeout.as_secs().clamp(1, 120);
    let args = vec![
        "fetch".to_string(),
        url.clone(),
        "--dump".to_string(),
        "text".to_string(),
        "--quiet".to_string(),
        "--timeout".to_string(),
        seconds.to_string(),
    ];
    let mut spec = CommandSpec::new(&exe, "obscura");
    spec.args = args;
    spec.timeout = timeout;
    spec.max_output_bytes = MAX_READ_BYTES;
    let out = runner::run_captured(spec).await?;
    if !out.success() {
        let last = runner::last_line(&out.stderr).trim();
        let message = if last.is_empty() {
            format!("obscura exited {}: no error output", out.code())
        } else {
            format!("obscura exited {}: {last}", out.code())
        };
        return Err(ReachError::new("UPSTREAM_BROKEN", message));
    }
    let text = out.stdout.trim();
    if text.is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "obscura returned no readable content",
        ));
    }
    Ok(vec![make_item(
        None,
        Some(url),
        None,
        None,
        Some(text.to_string()),
        json!({"backend": "obscura", "dump": "text"}),
    )])
}

/// Remove the obscura tool: deletes only regular files recorded under
/// `tools/obscura/bin/` with `owner: arana`; anything else is left untouched
/// and listed. Never follows a symlink or junction, including ancestors.
pub fn remove_obscura(state_dir: &Path) -> Result<Value> {
    let (tool_dir, bin_dir) = tool_paths(state_dir);
    let _lock = reach_core::state::FileLock::try_acquire(state_dir, OBSCURA_LOCK)?;
    let store = reach_core::state::Store::new(state_dir);
    let entry = store.load()?["tools"]["obscura"].clone();
    if !entry.is_object() {
        return Ok(json!({
            "tool": "obscura",
            "removed": false,
            "reason": "not installed",
        }));
    }
    if entry["owner"] != "arana" {
        let left = list_dir_files(&tool_dir);
        return Ok(json!({
            "tool": "obscura",
            "removed": false,
            "reason": format!("owner is {}", entry["owner"]),
            "left_untouched": left,
        }));
    }

    let mut deleted = 0usize;
    let mut skipped: Vec<Value> = Vec::new();
    if let Some(files) = entry["files"].as_array() {
        for relative in files {
            let Some(relative) = relative.as_str() else {
                continue;
            };
            // Only the tool's own bin subtree is owned; forged entries that
            // point at state config or other tool directories are refused.
            let Some(relative_path) = owned_bin_relative(relative) else {
                skipped.push(Value::String(relative.to_string()));
                continue;
            };
            let path = bin_dir.join(&relative_path);
            // Refuse if any ancestor (or the file itself) is a link/reparse
            // point, so removal can never be redirected outside the tool tree.
            if reach_core::state::ensure_safe(&path).is_err() {
                skipped.push(Value::String(relative.to_string()));
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() && !fsguard::is_link_like(&meta) => {
                    if std::fs::remove_file(&path).is_ok() {
                        deleted += 1;
                    }
                }
                _ => skipped.push(Value::String(relative.to_string())),
            }
        }
    }

    // Drop the ledger entry.
    store.mutate(|v| {
        if let Some(tools) = v["tools"].as_object_mut() {
            tools.remove("obscura");
        }
    })?;

    // Report (and leave) everything that is still in the tool directory.
    let left = list_dir_files(&tool_dir);
    // Prune now-empty directories we own, never touching links or leftovers.
    prune_empty_dirs(&tool_dir);
    Ok(json!({
        "tool": "obscura",
        "removed": true,
        "deleted": deleted,
        "left_untouched": left,
        "skipped_entries": skipped,
    }))
}

fn list_dir_files(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    // The tool root itself may be a junction/symlink planted in state; never
    // read through it.
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

fn prune_empty_dirs(root: &Path) {
    let mut stack = Vec::new();
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
                stack.push(path);
            }
        }
    }
    // Deepest first.
    stack.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in stack {
        if !fsguard::is_real_dir(&dir) || dir_is_non_empty(&dir) {
            continue;
        }
        let _ = std::fs::remove_dir(&dir);
    }
    if fsguard::is_real_dir(root) && !dir_is_non_empty(root) {
        let _ = std::fs::remove_dir(root);
    }
}
