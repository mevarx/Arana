//! Management vertical slices: real child processes (a compiled local fake
//! npm/python), the ownership ledger, version policy, inspection, refresh and
//! guarded removal. No network and no process-global environment changes.

use reach_tools::{management, registry};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// A tiny real executable used as npm/python/node/uv/ffmpeg. Compiled once per
/// test process with the local toolchain, so installers are exercised through
/// actual `Command` spawns rather than mocks.
const FAKE_INSTALLER: &str = r#"
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let exe = env::current_exe().expect("current exe");
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let args: Vec<String> = env::args().skip(1).collect();
    match stem.as_str() {
        "npm" => fake_npm(&args),
        "node" => println!("v24.14.1"),
        "uv" => println!("uv 0.13.9 (fake)"),
        "ffmpeg" => println!("ffmpeg version 7.1.1-fake"),
        "yt-dlp" => println!("2026.8.19"),
        "python" => fake_python(&exe, &args),
        _ => {}
    }
    ExitCode::SUCCESS
}

fn fake_npm(args: &[String]) {
    let Some(prefix) = args
        .iter()
        .position(|a| a == "--prefix")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
    else {
        return;
    };
    let spec = args.last().cloned().unwrap_or_default();
    let (package, version) = split_spec(&spec);
    let package_dir = prefix.join("node_modules").join(&package);
    fs::create_dir_all(&package_dir).expect("package dir");
    let manifest = format!("{{\"name\":\"{package}\",\"version\":\"{version}\"}}");
    fs::write(package_dir.join("package.json"), &manifest).expect("package.json");
    let bin = prefix.join("node_modules").join(".bin");
    fs::create_dir_all(&bin).expect("bin dir");
    let command = package.rsplit('/').next().unwrap_or("tool");
    let shim = format!("@echo off\r\necho {version}\r\n");
    fs::write(bin.join(format!("{command}.cmd")), &shim).expect("cmd shim");
    fs::write(bin.join(command), &shim).expect("sh shim");
    if args.iter().any(|a| a == "-g") {
        fs::write(prefix.join(format!("{command}.cmd")), &shim).expect("global shim");
        let global = prefix.join("lib").join("node_modules").join(&package);
        fs::create_dir_all(&global).expect("global package dir");
        fs::write(global.join("package.json"), manifest).expect("global package.json");
    }
}

fn split_spec(spec: &str) -> (String, String) {
    if let Some(rest) = spec.strip_prefix('@') {
        if let Some((name, version)) = rest.rsplit_once('@') {
            return (format!("@{name}"), version.to_string());
        }
    }
    match spec.rsplit_once('@') {
        Some((name, version)) => (name.to_string(), version.to_string()),
        None => (spec.to_string(), "0.0.0".to_string()),
    }
}

fn venv_site(exe: &Path) -> PathBuf {
    let venv = exe.parent().and_then(Path::parent).expect("venv root");
    if cfg!(windows) {
        venv.join("Lib").join("site-packages")
    } else {
        venv.join("lib").join("python3.13").join("site-packages")
    }
}

fn fake_python(exe: &Path, args: &[String]) {
    if args.first().map(String::as_str) == Some("-m")
        && args.get(1).map(String::as_str) == Some("venv")
    {
        let venv = PathBuf::from(args.get(2).cloned().unwrap_or_default());
        let scripts = if cfg!(windows) {
            venv.join("Scripts")
        } else {
            venv.join("bin")
        };
        fs::create_dir_all(&scripts).expect("scripts dir");
        let target = scripts.join(if cfg!(windows) { "python.exe" } else { "python" });
        fs::copy(exe, &target).expect("venv python");
        fs::write(venv.join("pyvenv.cfg"), "home = fake\n").expect("pyvenv.cfg");
        return;
    }
    if args.first().map(String::as_str) == Some("-c") {
        let fake = venv_site(exe)
            .join("imageio_ffmpeg")
            .join("binaries")
            .join(if cfg!(windows) { "ffmpeg-fake.exe" } else { "ffmpeg-fake" });
        fs::create_dir_all(fake.parent().unwrap()).expect("ffmpeg dir");
        fs::copy(exe, &fake).expect("ffmpeg fake");
        println!("{}", fake.display());
        return;
    }
    let spec = args.last().cloned().unwrap_or_default();
    let (dist, version, command) = if spec.contains("bilibili-cli") {
        ("bilibili_cli", "0.6.2", "bili")
    } else if spec.contains("imageio-ffmpeg") {
        ("imageio_ffmpeg", "0.8.1", "")
    } else if spec.contains("cloakbrowser") {
        ("cloakbrowser", "0.6.3", "cloakbrowser")
    } else if spec.contains("yt-dlp") {
        ("yt_dlp", "2026.8.19", "yt-dlp")
    } else {
        ("unknown", "0.0.0", "")
    };
    let site = venv_site(exe);
    fs::create_dir_all(site.join(format!("{dist}-{version}.dist-info"))).expect("dist-info");
    if !command.is_empty() {
        let scripts = exe.parent().expect("scripts");
        let target = scripts.join(exe_name(command));
        fs::copy(exe, target).expect("console script");
    }
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}
"#;

fn fake_installer() -> &'static Path {
    static FAKE: OnceLock<PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("arana-fake-installer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("fake dir");
        let source = dir.join("fake_installer.rs");
        fs::write(&source, FAKE_INSTALLER).expect("fake source");
        let output = dir.join(exe_name("npm"));
        let status = std::process::Command::new("rustc")
            .args(["--edition", "2021", "-O"])
            .arg(&source)
            .arg("-o")
            .arg(&output)
            .status()
            .expect("rustc is available for the fake installer");
        assert!(status.success(), "fake installer compiles");
        for name in ["node", "uv", "python", "ffmpeg", "opencli", "yt-dlp"] {
            fs::copy(&output, dir.join(exe_name(name))).expect("fake copy");
        }
        dir
    })
}

fn fake(name: &str) -> PathBuf {
    fake_installer().join(exe_name(name))
}

fn write_config(root: &Path, entries: &[(&str, &Path)]) {
    let mut text = String::new();
    for (key, value) in entries {
        text.push_str(&format!(
            "{key}: \"{}\"\n",
            value.to_string_lossy().replace('\\', "/")
        ));
    }
    fs::write(root.join("config.yaml"), text).expect("config.yaml");
}

/// A real script that reports `version` for `--version` probes, used as a
/// system yt-dlp copy (the compiled fake installer has one fixed version).
fn write_version_script(path: &Path, version: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    if cfg!(windows) {
        fs::write(path, format!("@echo off\r\necho {version}\r\n")).unwrap();
    } else {
        fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o700);
            fs::set_permissions(path, perms).unwrap();
        }
    }
}

fn node_overrides(root: &Path) {
    write_config(
        root,
        &[
            ("tool_node_bin", &fake("node")),
            ("tool_npm_bin", &fake("npm")),
            ("tool_npx_bin", &fake("node")),
        ],
    );
}

fn ledger(root: &Path, name: &str) -> Value {
    reach_core::state::Store::new(root)
        .load()
        .expect("state")
        .get("tools")
        .and_then(|tools| tools.get(name))
        .cloned()
        .unwrap_or(Value::Null)
}

fn record(root: &Path, name: &str, entry: Value) {
    registry::record(root, name, entry).expect("record");
}

#[test]
fn registry_pins_match_upstream_and_policy_specs() {
    let bili = registry::tool("bili-cli").expect("bili-cli");
    assert_eq!(bili.kind, registry::ToolKind::Pip);
    assert_eq!(bili.tested, Some("0.6.2"));
    assert_eq!(registry::install_spec(bili), "bilibili-cli==0.6.2");

    let boss = registry::tool("boss-agent-cli").expect("boss-agent-cli");
    assert!(boss
        .package
        .contains("github.com/can4hou6joeng4/boss-agent-cli/archive/"));
    assert_eq!(
        registry::install_spec(boss),
        boss.package,
        "the pinned GitHub archive is never rewritten as a PyPI pin"
    );

    let cloak = registry::tool("cloakbrowser").expect("cloakbrowser");
    assert_eq!(cloak.tested, Some("0.6.3"));

    let opencli = registry::tool("opencli").expect("opencli");
    assert!(opencli.singleton);
    assert_eq!(opencli.tested, Some("2.0.3"));
    assert_eq!(opencli.floor, Some("1.8.7"));
    assert_eq!(opencli.exclude, ["1.8.8"]);
    assert_eq!(registry::install_spec(opencli), "@jackwener/opencli@2.0.3");
    assert!(registry::allowed(opencli, "1.8.7"));
    assert!(!registry::allowed(opencli, "1.8.8"));
    assert!(!registry::allowed(opencli, "1.8.6"));
    assert!(registry::allowed(opencli, "2.0.3"));

    for prereq in ["node", "uv", "ffmpeg", "chrome"] {
        assert!(registry::tool(prereq).is_some(), "{prereq} is registered");
    }
    let yt_dlp = registry::tool("yt-dlp").expect("yt-dlp is registered");
    assert_eq!(yt_dlp.kind, registry::ToolKind::Pip);
    assert_eq!(yt_dlp.package, "yt-dlp");
    assert_eq!(yt_dlp.tested, Some("2026.8.19"));
    assert_eq!(yt_dlp.floor, Some("2026.1.1"));
    assert_eq!(registry::install_spec(yt_dlp), "yt-dlp==2026.8.19");
    assert_eq!(
        registry::tool("ffmpeg").map(|spec| spec.kind),
        Some(registry::ToolKind::Ffmpeg)
    );
}

#[tokio::test]
async fn ensure_opencli_installs_into_private_npm_prefix_and_inspects() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    node_overrides(root);

    let installed = management::ensure_tool("opencli", root, std::time::Duration::from_secs(120))
        .await
        .expect("opencli installs");
    assert_eq!(installed["tool"], "opencli");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["scope"], "global");
    assert_eq!(installed["version"], "2.0.3");
    assert!(root
        .join("tools/npm-global/node_modules/@jackwener/opencli/package.json")
        .is_file());
    assert_eq!(ledger(root, "opencli")["owner"], "arana");

    let inspected = management::inspect_tool("opencli", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["allowed"], true);
    assert_eq!(inspected["version"], "2.0.3");
    assert_eq!(inspected["owner"], "arana");
    assert_eq!(inspected["commands"][0]["source"], "private");

    // Idempotent: the same ledger record comes back (no reinstall timestamp).
    let installed_at = installed["installed_at"].clone();
    let again = management::ensure_tool("opencli", root, std::time::Duration::from_secs(120))
        .await
        .expect("second ensure");
    assert_eq!(again["installed_at"], installed_at);

    let removed = management::remove_tool("opencli", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(!root.join("tools/npm-global").exists());
    assert_eq!(ledger(root, "opencli"), Value::Null);
    let missing = management::inspect_tool("opencli", root)
        .await
        .expect("inspect missing");
    assert_eq!(missing["installed"], false);
}

#[tokio::test]
async fn user_singleton_is_reused_without_changes_and_outside_limits_is_refused() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let user = root.join("user-bin");
    fs::create_dir_all(&user).unwrap();
    let shim = user.join(if cfg!(windows) {
        "opencli.cmd"
    } else {
        "opencli"
    });
    let write = |version: &str| {
        if cfg!(windows) {
            fs::write(&shim, format!("@echo off\r\necho {version}\r\n")).unwrap();
        } else {
            fs::write(&shim, format!("#!/bin/sh\necho {version}\n")).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&shim).unwrap().permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&shim, perms).unwrap();
        }
    };
    write("1.9.0");
    write_config(
        root,
        &[
            ("tool_opencli_bin", &shim),
            ("tool_node_bin", &fake("node")),
            ("tool_npm_bin", &fake("npm")),
            ("tool_npx_bin", &fake("node")),
        ],
    );

    let record_value = management::ensure_tool("opencli", root, std::time::Duration::from_secs(60))
        .await
        .expect("user copy reused");
    assert_eq!(record_value["owner"], "user");
    assert_eq!(record_value["version"], "1.9.0");
    assert!(!root.join("tools/npm-global").exists(), "no private shadow");

    write("1.8.8");
    let error = management::ensure_tool("opencli", root, std::time::Duration::from_secs(60))
        .await
        .expect_err("excluded version refused");
    assert_eq!(error.code, "NEED_SETUP");

    write("1.0.0");
    let error = management::ensure_tool("opencli", root, std::time::Duration::from_secs(60))
        .await
        .expect_err("below floor refused");
    assert_eq!(error.code, "NEED_SETUP");

    let removed = management::remove_tool("opencli", root).expect("remove user");
    assert_eq!(removed["removed"], false);
    assert!(shim.is_file(), "the user copy is never deleted");
}

#[tokio::test]
async fn ensure_pip_tool_install_update_and_remove() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    write_config(root, &[("python_bin", &fake("python"))]);

    let installed = management::ensure_tool("bili-cli", root, std::time::Duration::from_secs(120))
        .await
        .expect("bili installs");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["scope"], "private");
    assert_eq!(installed["version"], "0.6.2");
    let launcher = registry::venv_bin_dir("bili-cli", root).join(exe_name("bili"));
    assert!(launcher.is_file(), "the console script exists");
    // Repair: a recorded install whose launcher vanished is reinstalled.
    fs::remove_file(&launcher).expect("drop launcher");
    let repaired = management::ensure_tool("bili-cli", root, std::time::Duration::from_secs(120))
        .await
        .expect("repair");
    assert_eq!(repaired["version"], "0.6.2");
    assert!(launcher.is_file(), "repair restored the console script");

    let inspected = management::inspect_tool("bili-cli", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["version"], "0.6.2");
    assert_eq!(inspected["commands"][0]["source"], "private");

    // Simulate an old recorded version; update refreshes to tested.
    reach_core::state::Store::new(root)
        .mutate(|value| value["tools"]["bili-cli"]["version"] = serde_json::json!("0.6.1"))
        .expect("tamper ledger");
    let updated = management::update_tool("bili-cli", root, std::time::Duration::from_secs(120))
        .await
        .expect("update");
    assert_eq!(updated["updated"], true);
    assert_eq!(updated["version"], "0.6.2");

    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(!root.join("tools/bili-cli").exists());
    assert_eq!(ledger(root, "bili-cli"), Value::Null);
    // A system `bili` may still be on PATH; inspect reports it as system and
    // never claims an arana-owned install remains.
    let after = management::inspect_tool("bili-cli", root)
        .await
        .expect("inspect after remove");
    assert!(
        after["owner"] == "system" || after["installed"] == false,
        "no arana record remains: {after}"
    );
}

/// yt-dlp is pinned as a private venv install (no externalCommand-only gap):
/// ensure/update/remove all work without touching the system copy.
#[tokio::test]
async fn ytdlp_private_pinned_install_update_inspect_and_remove() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    // A below-floor system copy keeps the private installer on its fallback
    // path (this machine may carry a modern yt-dlp on PATH); the override is
    // dropped after install so private resolution is observable.
    let shim = root.join("user-bin").join(if cfg!(windows) {
        "yt-dlp.cmd"
    } else {
        "yt-dlp"
    });
    write_version_script(&shim, "2025.1.1");
    write_config(
        root,
        &[("tool_yt-dlp_bin", &shim), ("python_bin", &fake("python"))],
    );

    let installed = management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(120))
        .await
        .expect("yt-dlp installs privately");
    assert_eq!(installed["kind"], "pip");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["scope"], "private");
    assert_eq!(installed["version"], "2026.8.19");
    let launcher = registry::venv_bin_dir("yt-dlp", root).join(exe_name("yt-dlp"));
    assert!(launcher.is_file(), "the private launcher exists");
    write_config(root, &[("python_bin", &fake("python"))]);

    let inspected = management::inspect_tool("yt-dlp", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["allowed"], true);
    assert_eq!(inspected["version"], "2026.8.19");
    assert_eq!(inspected["commands"][0]["source"], "private");

    reach_core::state::Store::new(root)
        .mutate(|value| value["tools"]["yt-dlp"]["version"] = serde_json::json!("2026.1.1"))
        .expect("tamper ledger");
    let updated = management::update_tool("yt-dlp", root, std::time::Duration::from_secs(120))
        .await
        .expect("update");
    assert_eq!(updated["updated"], true);
    assert_eq!(updated["version"], "2026.8.19");

    let removed = management::remove_tool("yt-dlp", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(!root.join("tools/yt-dlp").exists());
    assert_eq!(ledger(root, "yt-dlp"), Value::Null);
}

/// A system yt-dlp inside the floor/exclude limits satisfies ensure/update
/// as-is: no private shadow and never a global install.
#[tokio::test]
async fn ytdlp_system_copy_within_limits_is_reused_untouched() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let shim = root.join("user-bin").join(if cfg!(windows) {
        "yt-dlp.cmd"
    } else {
        "yt-dlp"
    });
    write_version_script(&shim, "2026.8.19");
    write_config(
        root,
        &[("tool_yt-dlp_bin", &shim), ("python_bin", &fake("python"))],
    );

    let reused = management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(60))
        .await
        .expect("system copy reused");
    assert_eq!(reused["owner"], "system");
    assert_eq!(reused["version"], "2026.8.19");
    assert!(
        !root.join("tools/yt-dlp").exists(),
        "no private shadow for an allowed system copy"
    );

    let updated = management::update_tool("yt-dlp", root, std::time::Duration::from_secs(60))
        .await
        .expect("update reports the system copy");
    assert_eq!(updated["updated"], false);
    assert_eq!(updated["owner"], "system");
    assert!(shim.is_file(), "the system copy is never modified");

    let inspected = management::inspect_tool("yt-dlp", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["owner"], "system");
    assert_eq!(inspected["allowed"], true);
}

/// A system yt-dlp below the floor triggers the private pinned fallback
/// instead of a global upgrade.
#[tokio::test]
async fn ytdlp_system_copy_outside_limits_gets_a_private_pin() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let shim = root.join("user-bin").join(if cfg!(windows) {
        "yt-dlp.cmd"
    } else {
        "yt-dlp"
    });
    write_version_script(&shim, "2025.1.1");
    write_config(
        root,
        &[("tool_yt-dlp_bin", &shim), ("python_bin", &fake("python"))],
    );

    let installed = management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(120))
        .await
        .expect("private pinned fallback");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["version"], "2026.8.19");
    let launcher = registry::venv_bin_dir("yt-dlp", root).join(exe_name("yt-dlp"));
    assert!(launcher.is_file());
    let system_text = fs::read_to_string(&shim).expect("system copy readable");
    assert!(
        system_text.contains("2025.1.1"),
        "the system copy is left untouched"
    );
}

#[tokio::test]
async fn prerequisites_reuse_system_copies_without_installing() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    write_config(
        root,
        &[
            ("tool_node_bin", &fake("node")),
            ("tool_npm_bin", &fake("npm")),
            ("tool_npx_bin", &fake("node")),
            ("tool_ffmpeg_bin", &fake("ffmpeg")),
        ],
    );

    let node = management::ensure_tool("node", root, std::time::Duration::from_secs(60))
        .await
        .expect("node");
    assert_eq!(node["owner"], "system");
    assert_eq!(node["version"], "24.14.1");
    assert!(!root.join("tools/node").exists());

    let ffmpeg = management::ensure_tool("ffmpeg", root, std::time::Duration::from_secs(60))
        .await
        .expect("ffmpeg");
    assert_eq!(ffmpeg["owner"], "system");
    assert_eq!(ffmpeg["version"], "7.1.1");
    assert!(!root.join("tools/ffmpeg").exists());

    // The update path reports the system copy rather than replacing it.
    let updated = management::update_tool("node", root, std::time::Duration::from_secs(60))
        .await
        .expect("update node");
    assert_eq!(updated["updated"], false);
    assert_eq!(updated["owner"], "system");
}

#[tokio::test]
async fn inspect_missing_is_not_an_error_and_unknown_names_are() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let missing = management::inspect_tool("cloakbrowser", root)
        .await
        .expect("missing is reported");
    assert_eq!(missing["installed"], false);
    assert_eq!(missing["tool"], "cloakbrowser");
    let error = management::inspect_tool("no-such-tool", root)
        .await
        .expect_err("unknown tool");
    assert_eq!(error.code, "BAD_INPUT");
}

#[test]
fn remove_ignores_forged_ledger_paths_and_user_owned_entries() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let secret = root.join("outside-secret.txt");
    fs::write(&secret, b"keep me").unwrap();
    fs::create_dir_all(root.join("tools/bili-cli")).unwrap();
    fs::write(root.join("tools/bili-cli/leftover.bin"), b"x").unwrap();
    record(
        root,
        "bili-cli",
        serde_json::json!({
            "owner": "arana",
            "scope": "private",
            "version": "0.6.2",
            "path": "tools/../../outside-secret.txt",
        }),
    );
    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(secret.is_file(), "a forged path never redirects deletion");
    assert!(!root.join("tools/bili-cli").exists());
    assert_eq!(ledger(root, "bili-cli"), Value::Null);

    fs::create_dir_all(root.join("tools/cloakbrowser")).unwrap();
    record(
        root,
        "cloakbrowser",
        serde_json::json!({"owner": "user", "scope": "private", "version": "0.6.3"}),
    );
    let left = management::remove_tool("cloakbrowser", root).expect("remove user");
    assert_eq!(left["removed"], false);
    assert!(root.join("tools/cloakbrowser").is_dir());
}

#[cfg(windows)]
#[test]
fn remove_refuses_a_junctioned_tool_directory() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"keep").unwrap();
    fs::create_dir_all(root.join("tools")).unwrap();
    let link = root.join("tools").join("bili-cli");
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(outside.to_string_lossy().replace('/', "\\"))
        .output()
        .expect("mklink runs");
    assert!(
        output.status.success(),
        "mklink /J fixture must be created: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(link.is_dir(), "the junction fixture exists");
    record(
        root,
        "bili-cli",
        serde_json::json!({"owner": "arana", "scope": "private", "version": "0.6.2"}),
    );
    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], false);
    assert!(
        outside.join("secret.txt").is_file(),
        "junction target survives"
    );
    assert_eq!(ledger(root, "bili-cli")["owner"], "arana");
}

#[test]
fn execution_env_prepends_private_bins_without_mutating_the_process() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let node_dir = registry::node_bin_dir(root);
    fs::create_dir_all(&node_dir).unwrap();
    fs::write(node_dir.join(exe_name("node")), b"stub").unwrap();
    let npm_bin = registry::npm_global_bin_dir(root);
    fs::create_dir_all(&npm_bin).unwrap();
    fs::write(
        npm_bin.join(if cfg!(windows) {
            "opencli.cmd".to_string()
        } else {
            "opencli".to_string()
        }),
        b"stub",
    )
    .unwrap();

    let before = std::env::var_os("PATH");
    let env = registry::execution_env("opencli", root);
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].0, "PATH");
    let parts: Vec<PathBuf> = std::env::split_paths(&env[0].1).collect();
    assert_eq!(parts[0], npm_bin, "private npm prefix is first");
    assert_eq!(parts[1], node_dir, "private node follows");
    assert_eq!(std::env::var_os("PATH"), before, "parent PATH unchanged");
    assert!(parts.len() > 2, "system PATH entries are preserved");
}

/// Official Unix Node tarballs omit usable `bin/npm`/`bin/npx` when their
/// symlink shims are skipped. Registry resolution must fall back to the
/// regular `lib/node_modules/npm/bin/*-cli.js` scripts and start them through
/// the private `node` runtime.
#[tokio::test]
async fn private_node_without_symlink_shims_resolves_npm_and_npx_through_node() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let node = registry::node_bin_dir(root).join(exe_name("node"));
    fs::create_dir_all(node.parent().unwrap()).unwrap();
    fs::write(&node, b"stub").unwrap();
    let cli_dir = registry::node_current_dir(root)
        .join("lib")
        .join("node_modules")
        .join("npm")
        .join("bin");
    fs::create_dir_all(&cli_dir).unwrap();
    for cli in ["npm-cli.js", "npx-cli.js"] {
        fs::write(cli_dir.join(cli), b"#!/usr/bin/env node\n").unwrap();
    }

    let node_spec = registry::tool("node").expect("node");
    for (command, cli) in [("npm", "npm-cli.js"), ("npx", "npx-cli.js")] {
        let script = cli_dir.join(cli);
        assert_eq!(
            registry::private_bin(node_spec, command, root),
            Some(script.clone()),
            "{command} resolves to its regular CLI script"
        );
        assert_eq!(registry::resolve(command, root), Some(script.clone()));
        let invocation = registry::resolve_invocation(command, root).expect("invocation");
        assert_eq!(
            invocation.program, node,
            "{command} runs through private node"
        );
        assert_eq!(
            invocation.args_prefix,
            vec![script.to_string_lossy().into_owned()],
            "{command} passes the CLI script before caller arguments"
        );
    }

    let inspected = management::inspect_tool("node", root)
        .await
        .expect("inspect private node");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["commands"][1]["command"], "npm");
    assert_eq!(inspected["commands"][1]["source"], "private");
    assert_eq!(inspected["commands"][2]["command"], "npx");
    assert_eq!(inspected["commands"][2]["source"], "private");
}

#[cfg(windows)]
#[tokio::test]
async fn batch_launchers_are_started_through_cmd_call() {
    let dir = tempfile::tempdir().expect("temp");
    let script = dir.path().join("hello.cmd");
    fs::write(&script, "@echo off\r\necho hello-%1\r\n").unwrap();
    let invocation = registry::invocation_for_path(script.clone());
    assert!(invocation
        .program
        .to_string_lossy()
        .to_lowercase()
        .ends_with("cmd.exe"));
    assert_eq!(invocation.args_prefix[0], "/C");
    assert_eq!(invocation.args_prefix[1], "call");
    let output = tokio::process::Command::new(&invocation.program)
        .args(&invocation.args_prefix)
        .arg("world")
        .output()
        .await
        .expect("cmd runs the batch shim");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello-world"), "stdout: {stdout}");
}

#[tokio::test]
async fn ensure_unknown_tool_is_bad_input() {
    let dir = tempfile::tempdir().expect("temp");
    let error = management::ensure_tool("nope", dir.path(), std::time::Duration::from_secs(5))
        .await
        .expect_err("unknown tool");
    assert_eq!(error.code, "BAD_INPUT");
}

/// Real network smoke: creates a private venv and installs the pinned
/// bilibili-cli wheel from PyPI, then inspects and removes it.
#[tokio::test]
#[ignore = "network: installs bilibili-cli 0.6.2 from PyPI"]
async fn live_private_bili_install_smoke() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let installed = management::ensure_tool("bili-cli", root, std::time::Duration::from_secs(300))
        .await
        .expect("live bili install");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["version"], "0.6.2");
    let inspected = management::inspect_tool("bili-cli", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["version"], "0.6.2");
    assert_eq!(inspected["allowed"], true);
    println!("live inspect: {inspected}");
    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], true);
}

/// Real network smoke for the singleton npm path: installs OpenCLI 2.0.3 into
/// the private `npm-global` prefix and checks the generated launcher resolves.
#[tokio::test]
#[ignore = "network: installs @jackwener/opencli 2.0.3 with the system npm"]
async fn live_private_opencli_install_smoke() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    let installed = management::ensure_tool("opencli", root, std::time::Duration::from_secs(300))
        .await
        .expect("live opencli install");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["version"], "2.0.3");
    let launcher =
        registry::private_bin(registry::tool("opencli").expect("opencli"), "opencli", root)
            .expect("private launcher");
    assert!(launcher.is_file());
    println!("private launcher: {}", launcher.display());
    let inspected = management::inspect_tool("opencli", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["version"], "2.0.3");
    println!("live inspect: {inspected}");
    let removed = management::remove_tool("opencli", root).expect("remove");
    assert_eq!(removed["removed"], true);
}

/// Real network smoke for the private yt-dlp fallback: installs the
/// pinned wheel into a private venv, inspects, updates and removes it. Never
/// installs globally and never touches a system copy.
#[tokio::test]
#[ignore = "network: installs yt-dlp 2026.8.19 from PyPI into a private venv"]
async fn live_private_ytdlp_install_inspect_update_remove_smoke() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    // A below-floor override keeps the real PyPI install on the private
    // fallback path on machines that already carry a modern yt-dlp on PATH.
    let shim = root.join("user-bin").join(if cfg!(windows) {
        "yt-dlp.cmd"
    } else {
        "yt-dlp"
    });
    write_version_script(&shim, "0.0.1");
    write_config(root, &[("tool_yt-dlp_bin", &shim)]);
    let installed = management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(300))
        .await
        .expect("live yt-dlp install");
    assert_eq!(installed["owner"], "arana");
    assert_eq!(installed["scope"], "private");
    assert_eq!(installed["version"], "2026.8.19");
    write_config(root, &[]);
    let inspected = management::inspect_tool("yt-dlp", root)
        .await
        .expect("inspect");
    assert_eq!(inspected["installed"], true);
    assert_eq!(inspected["allowed"], true);
    assert_eq!(inspected["version"], "2026.8.19");
    println!("live inspect: {inspected}");
    let updated = management::update_tool("yt-dlp", root, std::time::Duration::from_secs(300))
        .await
        .expect("live yt-dlp update");
    assert_eq!(updated["updated"], true);
    assert_eq!(updated["version"], "2026.8.19");
    let removed = management::remove_tool("yt-dlp", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(!root.join("tools/yt-dlp").exists());
}

/// Regression (Luna review): a file the user drops inside an arana-owned tool
/// directory is not ours to delete. Remove deletes only manifest-recorded
/// files and lists the addition under `left_untouched`.
#[tokio::test]
async fn remove_preserves_files_added_after_install_and_lists_them() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    write_config(root, &[("python_bin", &fake("python"))]);
    management::ensure_tool("bili-cli", root, std::time::Duration::from_secs(120))
        .await
        .expect("install");
    let user_file = root.join("tools/bili-cli/user-notes.txt");
    fs::write(&user_file, "user data").expect("user file");

    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(
        user_file.is_file(),
        "an unrecorded user file must survive tool removal"
    );
    let left: Vec<String> = removed["left_untouched"]
        .as_array()
        .expect("left_untouched list")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(
        left.iter().any(|p| p.ends_with("user-notes.txt")),
        "the surviving user file must be listed: {left:?}"
    );
    assert!(
        !root.join("tools/bili-cli/.bilibili-stuff").exists(),
        "sanity anchor"
    );
}

/// A manifest-less owned tree (legacy state or a tampered sidecar) keeps the
/// documented wholesale fallback: the whole owned directory is removed.
#[tokio::test]
async fn remove_falls_back_to_wholesale_without_a_manifest() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    write_config(root, &[("python_bin", &fake("python"))]);
    management::ensure_tool("bili-cli", root, std::time::Duration::from_secs(120))
        .await
        .expect("install");
    fs::remove_file(root.join("tools/bili-cli/.arana-manifest.json")).expect("drop manifest");

    let removed = management::remove_tool("bili-cli", root).expect("remove");
    assert_eq!(removed["removed"], true);
    assert!(!root.join("tools/bili-cli").exists());
}

/// Regression (Luna review): an owned install whose recorded version sits
/// outside floor/exclude is repaired to the tested pin, never accepted.
#[tokio::test]
async fn owned_install_with_out_of_limits_version_is_repaired_not_accepted() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    // A below-floor system copy forces the private venv path.
    let shim = root.join("user-bin").join(if cfg!(windows) {
        "yt-dlp.cmd"
    } else {
        "yt-dlp"
    });
    write_version_script(&shim, "2025.1.1");
    write_config(
        root,
        &[("tool_yt-dlp_bin", &shim), ("python_bin", &fake("python"))],
    );
    management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(120))
        .await
        .expect("private install");
    write_config(root, &[("python_bin", &fake("python"))]);
    let launcher = registry::venv_bin_dir("yt-dlp", root).join(exe_name("yt-dlp"));
    assert!(launcher.is_file());

    // A stale/tampered ledger version below the floor with the launcher intact.
    reach_core::state::Store::new(root)
        .mutate(|value| value["tools"]["yt-dlp"]["version"] = serde_json::json!("2025.1.1"))
        .expect("tamper ledger");

    let ensured = management::ensure_tool("yt-dlp", root, std::time::Duration::from_secs(120))
        .await
        .expect("repair to tested");
    assert_eq!(
        ensured["version"], "2026.8.19",
        "out-of-limits owned version must not be accepted as-is: {ensured}"
    );
    assert!(
        launcher.is_file(),
        "the private launcher survives the repair"
    );
}
