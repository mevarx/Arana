//! Registry resolution and config access tests.

use reach_tools::{config, registry};

struct EnvGuard(Vec<(String, Option<String>)>);

impl EnvGuard {
    fn set(keys: &[&str]) -> Self {
        let mut saved = Vec::new();
        for key in keys {
            saved.push((key.to_string(), std::env::var(key).ok()));
            std::env::remove_var(key);
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

#[test]
fn registry_pins_obscura_as_a_pinned_binary_tool() {
    let tools = registry::tools();
    let obscura = tools
        .iter()
        .find(|t| t.name == "obscura")
        .expect("obscura is registered");
    assert_eq!(obscura.kind, registry::ToolKind::Binary);
    assert_eq!(obscura.tested, Some("0.2.7"));
    assert!(obscura.commands.contains(&"obscura"));
    // The external command tools used by the media channel.
    assert!(tools.iter().any(|t| t.name == "yt-dlp"));
    assert!(tools.iter().any(|t| t.name == "ffmpeg"));
}

#[test]
fn resolve_prefers_env_override_then_private_then_path() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let _env = EnvGuard::set(&["ARANA_OBSCURA_BIN", "ARANA_FFMPEG_BIN"]);

    // Unknown commands never resolve.
    assert!(registry::resolve("definitely-not-a-command-xyz", state).is_none());

    // A private install under state_dir/tools/<name>/bin wins over PATH, even
    // when the command also exists on the system PATH.
    let bin = registry::tool_bin_dir("ffmpeg", state);
    std::fs::create_dir_all(&bin).unwrap();
    let exe = if cfg!(windows) {
        bin.join("ffmpeg.exe")
    } else {
        bin.join("ffmpeg")
    };
    std::fs::write(&exe, b"stub").unwrap();
    assert_eq!(registry::resolve("ffmpeg", state), Some(exe));

    // The env override wins over everything.
    let custom = dir.path().join("custom-ffmpeg");
    std::fs::write(&custom, b"stub").unwrap();
    std::env::set_var("ARANA_FFMPEG_BIN", &custom);
    assert_eq!(registry::resolve("ffmpeg", state), Some(custom));

    // Without any private install the system PATH is used (this machine has
    // ffmpeg via winget; on a clean machine the result is simply None).
    let clean = tempfile::tempdir().unwrap();
    if let Some(path) = registry::resolve("ffmpeg", clean.path()) {
        assert!(
            !path.starts_with(clean.path()),
            "must not invent private paths"
        );
    }
}

#[test]
fn version_limits_follow_floor_and_exclude() {
    let tools = registry::tools();
    let yt_dlp = tools.iter().find(|t| t.name == "yt-dlp").unwrap();
    // tested version is always allowed
    assert!(registry::allowed(yt_dlp, "2026.8.19"));
    // garbage is never allowed
    assert!(!registry::allowed(yt_dlp, "not a version"));
    // below floor is rejected
    assert!(!registry::allowed(yt_dlp, "0.1.0"));
    assert_eq!(
        registry::version_tuple("yt-dlp 2026.8.19 (bin)"),
        Some((2026, 8, 19))
    );
}

#[tokio::test]
async fn record_writes_the_ownership_ledger_into_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let entry = registry::record(
        state,
        "obscura",
        serde_json::json!({"owner": "arana", "scope": "private", "version": "0.2.7"}),
    )
    .expect("record succeeds");
    assert_eq!(entry["tool"], "obscura");
    assert_eq!(entry["owner"], "arana");
    // The state.json under the state dir carries the tools ledger.
    let store = reach_core::state::Store::new(state);
    let data = store.load().unwrap();
    assert_eq!(data["tools"]["obscura"]["owner"], "arana");
    assert_eq!(data["tools"]["obscura"]["version"], "0.2.7");
}

#[test]
fn config_reads_yaml_keys_and_env_fallback_without_creating_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let _env = EnvGuard::set(&["GROQ_API_KEY", "OPENAI_API_KEY"]);

    // Missing config → no value, and no file is created by a read.
    assert_eq!(config::get(state, "groq_api_key"), None);
    assert!(!state.join("config.yaml").exists());

    // File value wins over the environment.
    std::fs::write(state.join("config.yaml"), "groq_api_key: file-key\n").unwrap();
    assert_eq!(config::get(state, "groq_api_key"), Some("file-key".into()));

    // Env fallback when the key is absent from the file.
    std::env::set_var("OPENAI_API_KEY", "env-key");
    assert_eq!(
        config::get(state, "openai_api_key"),
        Some("env-key".into()),
        "uppercase env var backs the config key"
    );
}

#[test]
fn config_rejects_non_mapping_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    std::fs::write(state.join("config.yaml"), "- just\n- a\n- list\n").unwrap();
    let err = config::read_config(state).expect_err("non-mapping config fails");
    assert_eq!(err.code, "BAD_INPUT");
}

#[test]
fn tool_bin_dir_is_under_the_state_tree() {
    let dir = tempfile::tempdir().unwrap();
    let bin = registry::tool_bin_dir("obscura", dir.path());
    assert_eq!(bin, dir.path().join("tools").join("obscura").join("bin"));
}
