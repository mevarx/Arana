//! Obscura install/read/remove: pinned-binary install with sha256
//! verification, ownership-ledger removal, and the `fetch --dump text`
//! read adapter.

use reach_tools::obscura::{install_obscura, obscura_read, remove_obscura};
use reach_tools::registry;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::Mutex;

/// Env-var mutations are process-global; serialize obscura tests.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── zip builder (STORED) ──

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::new();
    let put16 = |buf: &mut Vec<u8>, v: u16| buf.extend_from_slice(&v.to_le_bytes());
    for (name, data) in entries {
        let offset = out.len() as u32;
        offsets.push(offset);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        put16(&mut out, 20);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, 0);
        out.extend_from_slice(&crc32(data).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
    }
    let cd_start = out.len() as u32;
    for ((name, data), offset) in entries.iter().zip(&offsets) {
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        put16(&mut central, 20);
        put16(&mut central, 20);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        central.extend_from_slice(&crc32(data).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        put16(&mut central, name.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, entries.len() as u16);
    put16(&mut out, entries.len() as u16);
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_start.to_le_bytes());
    put16(&mut out, 0);
    out
}

// ── tar.gz builder (unix payloads) ──

/// Build a gzip-compressed tar with explicit unix modes: the production tar
/// extractor applies each header's mode, so a 0o755 member is executable.
fn build_tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut encoder);
        for (name, data, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(*mode);
            header.set_size(data.len() as u64);
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

/// The main executable's file name on this platform, as `install_obscura`
/// writes it.
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "obscura.exe"
    } else {
        "obscura"
    }
}

/// The asset bytes the fixture registry serves: a stealth-archive stand-in
/// with both executables and a bundled runtime DLL. It deliberately carries no
/// NOTICE, like the audited upstream archive. Windows publishes a zip with
/// `.exe` names; every unix release publishes a tar.gz of bare names carrying
/// the executable bit, so the fixture mirrors the asset the installer actually
/// selects for this platform.
fn fixture_archive() -> Vec<u8> {
    if cfg!(windows) {
        build_zip(&[
            ("obscura.exe", b"MZ-fake-obscura-main"),
            ("obscura-worker.exe", b"MZ-fake-obscura-worker"),
            ("vcruntime140.dll", b"fake-vc-runtime-dll"),
        ])
    } else {
        build_tar_gz(&[
            ("obscura", b"ELF-fake-obscura-main", 0o755),
            ("obscura-worker", b"ELF-fake-obscura-worker", 0o755),
            ("vcruntime140.dll", b"fake-vc-runtime-dll", 0o644),
        ])
    }
}

// ── HTTP fixture server ──

struct Server {
    port: u16,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn serve(body: Vec<u8>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 2048];
                let Ok(n) = std::io::Read::read(&mut stream, &mut buf) else {
                    continue;
                };
                let _req = String::from_utf8_lossy(&buf[..n]);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            port,
            handle: Some(handle),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.handle.take();
    }
}

struct EnvReset(Vec<(&'static str, Option<String>)>);

impl EnvReset {
    fn save(keys: &[&'static str]) -> Self {
        let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        Self(saved)
    }
}

impl Drop for EnvReset {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// The real asset name for this platform, as the registry will request it.
fn asset_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "obscura-x86_64-windows-stealth.zip",
        ("linux", "x86_64") => "obscura-x86_64-linux-stealth.tar.gz",
        ("linux", "aarch64") => "obscura-aarch64-linux-stealth.tar.gz",
        ("macos", "x86_64") => "obscura-x86_64-macos-stealth.tar.gz",
        ("macos", "aarch64") => "obscura-aarch64-macos-stealth.tar.gz",
        _ => "obscura-unknown.tar.gz",
    }
}

/// Create a directory link without administrator rights: a junction on
/// Windows, a symlink elsewhere.
fn make_dir_link(link: &Path, target: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .output()
            .expect("cmd runs");
        assert!(
            output.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
    }
}

#[tokio::test]
async fn installs_the_pinned_release_and_is_idempotent() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive.clone());
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", hex(&Sha256::digest(&archive)));

    let record = install_obscura(state).await.expect("install succeeds");
    assert_eq!(record["tool"], "obscura");
    assert_eq!(record["owner"], "arana");
    assert_eq!(record["scope"], "private");
    assert_eq!(record["version"], "0.2.7");
    assert_eq!(record["archive"], asset_name());

    // The binaries (and the DLL — FR: archive DLLs kept) land in the
    // private bin dir and resolve through the registry.
    let bin = registry::tool_bin_dir("obscura", state);
    assert!(bin.join(exe_name()).exists());
    // The unix asset carries the executable bit; the installer must preserve it
    // or the resolved binary cannot be spawned.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(bin.join(exe_name()))
            .expect("installed binary")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "executable mode: {mode:o}");
    }
    assert!(
        bin.join("vcruntime140.dll").exists(),
        "bundled DLLs are kept"
    );
    assert_eq!(
        registry::resolve("obscura", state),
        Some(bin.join(exe_name()))
    );

    // The state ledger records every installed file, including the audited
    // license and the local attribution installed when the archive has no
    // NOTICE.
    let store = reach_core::state::Store::new(state);
    let tools = store.load().unwrap()["tools"]["obscura"].clone();
    assert_eq!(tools["owner"], "arana");
    let files = tools["files"].as_array().unwrap();
    assert!(files.len() >= 5, "files recorded: {files:?}");
    assert!(files
        .iter()
        .all(|f| f.as_str().unwrap_or("").starts_with("tools/obscura/bin")));
    assert!(files
        .iter()
        .any(|f| f.as_str().unwrap_or("").ends_with("/LICENSE")));
    assert!(files
        .iter()
        .any(|f| f.as_str().unwrap_or("").ends_with("/NOTICE")));
    let license = std::fs::read_to_string(bin.join("LICENSE")).expect("license installed");
    assert_eq!(
        license,
        include_str!("../../../THIRD_PARTY/obscura/LICENSE")
    );
    let notice = std::fs::read_to_string(bin.join("NOTICE")).expect("local attribution installed");
    assert!(notice.contains("obscura 0.2.7"), "{notice}");
    assert!(notice.contains("Apache License, Version 2.0"), "{notice}");

    // A second install is a no-op returning the same record.
    let again = install_obscura(state).await.expect("reinstall succeeds");
    assert_eq!(again["version"], "0.2.7");
    assert_eq!(again["owner"], "arana");
}

#[tokio::test]
async fn install_refuses_an_occupied_unrecorded_directory() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let stranger = registry::tool_bin_dir("obscura", state).join("stranger.txt");
    std::fs::create_dir_all(stranger.parent().unwrap()).unwrap();
    std::fs::write(&stranger, b"user data").unwrap();

    let err = install_obscura(state)
        .await
        .expect_err("occupied dir refused");
    assert_eq!(err.code, "NEED_SETUP");
    assert!(err.message.contains("Unrecorded"), "{}", err.message);
    // The user's file is left untouched.
    assert_eq!(std::fs::read(&stranger).unwrap(), b"user data");
}

#[tokio::test]
async fn install_rejects_a_checksum_mismatch() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", "0".repeat(64));

    let err = install_obscura(state)
        .await
        .expect_err("sha mismatch fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("checksum"), "{}", err.message);
    // Nothing was extracted or recorded.
    assert!(!registry::tool_bin_dir("obscura", state)
        .join(exe_name())
        .exists());
    let store = reach_core::state::Store::new(state);
    assert!(store.load().unwrap()["tools"]["obscura"].is_null());
}

#[tokio::test]
async fn remove_deletes_only_owned_files_and_reports_the_rest() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive.clone());
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", hex(&Sha256::digest(&archive)));
    install_obscura(state).await.expect("install succeeds");

    // A file the user dropped in is not ours.
    let bin = registry::tool_bin_dir("obscura", state);
    std::fs::write(bin.join("user-notes.txt"), b"keep me").unwrap();

    let summary = remove_obscura(state).expect("remove succeeds");
    assert_eq!(summary["removed"], true, "{summary}");
    assert_eq!(summary["tool"], "obscura");
    let left: Vec<&str> = summary["left_untouched"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        left.iter().any(|f| f.contains("user-notes.txt")),
        "{summary}"
    );
    assert!(left.iter().all(|f| !f.contains(exe_name())), "{summary}");
    // Owned files are gone; the user's file survives.
    assert!(!bin.join(exe_name()).exists());
    assert!(
        !bin.join("LICENSE").exists(),
        "the installed license is removed"
    );
    assert!(
        !bin.join("NOTICE").exists(),
        "the installed attribution is removed"
    );
    assert!(bin.join("user-notes.txt").exists());
    // The ledger entry is gone.
    let store = reach_core::state::Store::new(state);
    assert!(store.load().unwrap()["tools"]["obscura"].is_null());

    // Removing again reports not-installed.
    let again = remove_obscura(state).expect("second remove is a no-op");
    assert_eq!(again["removed"], false, "{again}");
}

#[tokio::test]
async fn remove_refuses_forged_entries_pointing_outside_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("precious.txt");
    std::fs::write(&victim, b"precious").unwrap();

    // A forged ledger entry with escaping paths must never be followed.
    reach_core::state::Store::new(state)
        .mutate(|v| {
            v["tools"]["obscura"] = serde_json::json!({
                "owner": "arana",
                "scope": "private",
                "version": "0.2.7",
                "files": [
                    "../../precious.txt",
                    format!("{}", victim.display()),
                    format!("tools/obscura/bin/{}", exe_name())
                ],
            });
        })
        .unwrap();
    std::fs::create_dir_all(registry::tool_bin_dir("obscura", state)).unwrap();

    let summary = remove_obscura(state).expect("remove handles forged entries");
    assert_eq!(summary["removed"], true);
    assert!(
        victim.exists(),
        "a file outside the state dir is never deleted"
    );
    assert!(!Path::new(&format!("{}/../../precious.txt", state.display())).exists());
}

#[tokio::test]
async fn remove_only_deletes_paths_inside_the_install_bin_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    // Both files are real, but neither is part of obscura's owned payload.
    let state_config = state.join("config.yaml");
    std::fs::write(&state_config, b"state config").unwrap();
    let tool_config = state.join("tools").join("obscura").join("config.yaml");
    std::fs::create_dir_all(tool_config.parent().unwrap()).unwrap();
    std::fs::write(&tool_config, b"tool config").unwrap();
    let bin = registry::tool_bin_dir("obscura", state);
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(exe_name());
    std::fs::write(&exe, b"owned binary").unwrap();

    reach_core::state::Store::new(state)
        .mutate(|v| {
            v["tools"]["obscura"] = serde_json::json!({
                "owner": "arana",
                "scope": "private",
                "version": "0.2.7",
                "files": [
                    "config.yaml",
                    "tools/obscura/config.yaml",
                    format!("tools/obscura/bin/{}", exe_name()),
                ],
            });
        })
        .unwrap();

    let summary = remove_obscura(state).expect("remove succeeds");
    assert_eq!(summary["removed"], true, "{summary}");
    assert!(
        state_config.exists(),
        "a bare relative ledger path is not ours: {summary}"
    );
    assert!(
        tool_config.exists(),
        "only the bin subtree is owned: {summary}"
    );
    assert!(!exe.exists(), "the recorded bin payload is removed");
    let skipped: Vec<&str> = summary["skipped_entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(skipped.contains(&"config.yaml"), "{summary}");
    assert!(skipped.contains(&"tools/obscura/config.yaml"), "{summary}");
}

#[tokio::test]
async fn remove_never_follows_a_linked_bin_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"precious").unwrap();
    // A directory that is empty only because it lives outside the tool tree.
    let outside_empty = outside.path().join("empty-dir");
    std::fs::create_dir_all(&outside_empty).unwrap();

    let bin = registry::tool_bin_dir("obscura", state);
    make_dir_link(&bin, outside.path());

    reach_core::state::Store::new(state)
        .mutate(|v| {
            v["tools"]["obscura"] = serde_json::json!({
                "owner": "arana",
                "scope": "private",
                "version": "0.2.7",
                "files": ["tools/obscura/bin/victim.txt"],
            });
        })
        .unwrap();

    let summary = remove_obscura(state).expect("remove reports instead of following links");
    assert_eq!(summary["removed"], true, "{summary}");
    assert!(
        victim.exists(),
        "removal must not delete through a junction: {summary}"
    );
    assert!(
        outside_empty.exists(),
        "pruning must not reach through a junction: {summary}"
    );
    let left = summary["left_untouched"].as_array().unwrap();
    assert!(
        left.iter()
            .all(|f| !f.as_str().unwrap_or("").contains("victim.txt")),
        "listing must not walk outside the tool tree: {summary}"
    );
}

#[tokio::test]
async fn install_and_remove_use_the_same_exclusive_lock() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    std::env::set_var("ARANA_TOOL_REGISTRY", "http://127.0.0.1:1");
    std::env::set_var("ARANA_OBSCURA_SHA256", "0".repeat(64));
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    // Same name the obscura installer/remover takes; no other command holds it.
    let held = reach_core::state::FileLock::try_acquire(state, "reach-tools-obscura")
        .expect("test takes the install lock");
    let err = remove_obscura(state).expect_err("remove must wait for an install");
    assert_eq!(err.code, "BUSY", "{}", err.message);
    let err = install_obscura(state)
        .await
        .expect_err("install must wait for another install/remove");
    assert_eq!(err.code, "BUSY", "{}", err.message);
    drop(held);
    let summary = remove_obscura(state).expect("lock release clears the way");
    assert_eq!(summary["removed"], false, "{summary}");
}

#[tokio::test]
async fn install_repairs_missing_recorded_files_and_stale_versions() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive.clone());
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", hex(&Sha256::digest(&archive)));
    install_obscura(state).await.expect("install succeeds");

    // A missing recorded file must trigger a repair, not an early return.
    let bin = registry::tool_bin_dir("obscura", state);
    let dll = bin.join("vcruntime140.dll");
    std::fs::remove_file(&dll).unwrap();
    install_obscura(state).await.expect("repair succeeds");
    assert!(dll.exists(), "repair restores recorded files");

    // A stale recorded version is repaired too.
    reach_core::state::Store::new(state)
        .mutate(|v| v["tools"]["obscura"]["version"] = serde_json::json!("0.2.6"))
        .unwrap();
    let record = install_obscura(state)
        .await
        .expect("version repair succeeds");
    assert_eq!(record["version"], "0.2.7", "{record}");
}

#[tokio::test]
async fn install_repair_refuses_to_overwrite_unrecorded_files() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive.clone());
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", hex(&Sha256::digest(&archive)));

    // A partial arana ledger with no recorded files: the existing binary was
    // not recorded, so repair must not replace it.
    let bin = registry::tool_bin_dir("obscura", state);
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(exe_name());
    std::fs::write(&exe, b"user-owned-binary").unwrap();
    reach_core::state::Store::new(state)
        .mutate(|v| {
            v["tools"]["obscura"] = serde_json::json!({
                "owner": "arana",
                "scope": "private",
                "version": "0.2.7",
                "files": [],
            });
        })
        .unwrap();

    let err = install_obscura(state)
        .await
        .expect_err("unrecorded target files are never overwritten");
    assert_eq!(err.code, "NEED_SETUP", "{}", err.message);
    assert!(err.message.contains("Unrecorded"), "{}", err.message);
    assert_eq!(std::fs::read(&exe).unwrap(), b"user-owned-binary");
}

#[tokio::test]
async fn install_never_clears_through_a_linked_tmp_directory() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let archive = fixture_archive();
    let server = Server::serve(archive.clone());
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let outside = tempfile::tempdir().unwrap();
    std::env::set_var(
        "ARANA_TOOL_REGISTRY",
        format!("http://127.0.0.1:{}", server.port),
    );
    std::env::set_var("ARANA_OBSCURA_SHA256", hex(&Sha256::digest(&archive)));

    // The old staging path is predictable; plant it behind a linked tmp dir.
    let stale = outside
        .path()
        .join(format!("obscura-install-{}", std::process::id()));
    std::fs::create_dir_all(&stale).unwrap();
    let keep = stale.join("keep.txt");
    std::fs::write(&keep, b"do not delete").unwrap();
    make_dir_link(&state.join("tmp"), outside.path());

    let err = install_obscura(state)
        .await
        .expect_err("a linked tmp directory is refused");
    assert_eq!(err.code, "BAD_INPUT", "{}", err.message);
    assert!(
        keep.exists(),
        "install must not clear a linked staging area"
    );
}

// ── obscura_read ──

/// A fixture obscura binary: records its argv beside itself, then prints the
/// "rendered" page text.
fn write_fake_obscura(dir: &Path) -> PathBuf {
    write_command(
        dir,
        "obscura",
        "@echo off\r\necho %* > \"%~dp0args.txt\"\r\necho Fixture rendered page text\r\n",
        "echo \"$@\" > \"$(dirname \"$0\")/args.txt\"\necho 'Fixture rendered page text'\n",
    )
}

/// Mark a fixture script executable so the adapter can spawn it directly.
#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// Write a spawnable fixture command: a batch file on Windows, a `#!/bin/sh`
/// script elsewhere (so the adapter's real spawn path is exercised on every
/// platform, not a shell wrapper).
fn write_command(dir: &Path, stem: &str, windows_body: &str, unix_body: &str) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{stem}.bat"));
        std::fs::write(&path, windows_body).unwrap();
        return path;
    }
    let path = dir.join(format!("{stem}.sh"));
    std::fs::write(&path, format!("#!/bin/sh\n{unix_body}")).unwrap();
    make_executable(&path);
    path
}

#[tokio::test]
async fn reads_a_page_through_the_obscura_adapter() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let bin = write_fake_obscura(dir.path());
    std::env::set_var("ARANA_OBSCURA_BIN", &bin);

    let items = obscura_read(
        "https://example.com/js-page",
        Duration::from_secs(20),
        state,
    )
    .await
    .expect("read succeeds");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["url"], "https://example.com/js-page");
    assert_eq!(items[0]["text"], "Fixture rendered page text");
    assert_eq!(items[0]["raw"]["backend"], "obscura");
    assert_eq!(items[0]["title"], Value::Null);

    // The exact argv the adapter builds (obscura CLI contract).
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    let args = args.replace(['"', '\r'], "");
    assert!(args.contains("fetch"), "{args}");
    assert!(args.contains("--dump text"), "{args}");
    assert!(args.contains("--quiet"), "{args}");
    assert!(args.contains("--timeout"), "{args}");
    assert!(args.contains("https://example.com/js-page"), "{args}");
    // The SSRF bypass flag is never forwarded.
    assert!(!args.contains("allow-private-network"), "{args}");
}

#[tokio::test]
async fn read_requires_the_binary_and_a_public_url() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    // No binary → NEED_SETUP.
    let err = obscura_read("https://example.com/x", Duration::from_secs(10), state)
        .await
        .expect_err("missing binary fails");
    assert_eq!(err.code, "NEED_SETUP");

    // Private/internal targets are rejected before the binary ever runs.
    let bin = write_fake_obscura(dir.path());
    std::env::set_var("ARANA_OBSCURA_BIN", &bin);
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.1/x",
        "https://internal/x",
        "ftp://example.com/x",
    ] {
        let err = obscura_read(url, Duration::from_secs(10), state)
            .await
            .expect_err("private url rejected");
        assert_eq!(err.code, "BAD_INPUT", "{url}: {}", err.message);
    }
    // The fixture never ran (no args file was produced).
    assert!(!dir.path().join("args.txt").exists());
}

#[tokio::test]
async fn read_maps_failures_and_empty_content() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvReset::save(&[
        "ARANA_TOOL_REGISTRY",
        "ARANA_OBSCURA_SHA256",
        "ARANA_OBSCURA_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    // Non-zero exit with stderr → UPSTREAM_BROKEN with the last line.
    let failing = write_command(
        dir.path(),
        "fail",
        "@echo off\r\necho engine exploded >&2\r\nexit /b 7\r\n",
        "echo 'engine exploded' >&2\nexit 7\n",
    );
    std::env::set_var("ARANA_OBSCURA_BIN", &failing);
    let err = obscura_read("https://example.com/x", Duration::from_secs(10), state)
        .await
        .expect_err("failure maps to UPSTREAM_BROKEN");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("exploded"), "{}", err.message);

    // Exit 0 but empty output → no readable content (falls through to jina).
    let empty = write_command(dir.path(), "empty", "@echo off\r\n", "exit 0\n");
    std::env::set_var("ARANA_OBSCURA_BIN", &empty);
    let err = obscura_read("https://example.com/x", Duration::from_secs(10), state)
        .await
        .expect_err("empty output is not content");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(
        err.message.contains("no readable content"),
        "{}",
        err.message
    );
}
