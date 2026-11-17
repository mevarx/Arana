use serde_json::Value;
use std::process::{Command, Output};
fn run(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(args)
        .env("ARANA_HOME", root)
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap()
}
#[test]
fn doctor_reports_all_channels_without_creating_state_or_claiming_live_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("absent");
    let out = run(&root, &["doctor"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let channels = value["result"]["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 14);
    assert!(channels.iter().all(|row| row["status"] != "ready"));
    assert!(!root.exists(), "doctor must be read-only");
}
#[test]
fn check_records_real_failed_attempt_and_previous_results_without_a_provider_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("state.json"),
        r#"{"checks":{"xiaoyuzhou":{"actions":{"transcribe":true}}}}"#,
    )
    .unwrap();
    let out = run(dir.path(), &["check", "xiaoyuzhou", "--timeout", "1"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!out.status.success());
    let rows = value["result"]["results"]
        .as_array()
        .expect("actual check rows");
    assert!(!rows.is_empty());
    assert_eq!(rows[0]["error"]["code"], "NEED_SETUP");
    assert_eq!(rows[0]["passed"], false);
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["checks"]["xiaoyuzhou"]["previous"]["transcribe"],
        true
    );
    assert_eq!(
        state["checks"]["xiaoyuzhou"]["actions"]["transcribe"],
        false
    );
}
#[test]
fn configure_rejects_blank_secret_before_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("absent");
    let out = run(&root, &["configure", "groq-key", "  "]);
    assert!(!out.status.success());
    assert!(!root.exists());
}
#[test]
fn configure_stdin_does_not_overwrite_another_keys_newer_value() {
    use std::{io::Write, process::Stdio, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert!(run(root, &["configure", "groq-key", "initial-dummy"])
        .status
        .success());
    let mut slow = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(["configure", "groq-key", "--stdin"])
        .env("ARANA_HOME", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = slow.stdin.take().unwrap();
    input.write_all(b"changed-dummy").unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert!(run(root, &["configure", "openai-key", "second-dummy"])
        .status
        .success());
    drop(input);
    assert!(slow.wait_with_output().unwrap().status.success());
    let config: Value =
        serde_yaml::from_str(&std::fs::read_to_string(root.join("config.yaml")).unwrap()).unwrap();
    assert_eq!(config["openai_api_key"], "second-dummy");
    assert_eq!(config["groq_api_key"], "changed-dummy");
}

#[test]
fn help_lists_the_complete_management_surface_without_a_beta_stub() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["help"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success());
    let usage = value["result"]["usage"].as_str().unwrap();
    for command in [
        "setup",
        "add",
        "remove",
        "update",
        "login",
        "logout",
        "doctor",
        "check",
        "uninstall",
        "skill",
        "configure",
        "browser",
    ] {
        assert!(usage.contains(command), "help omits {command}: {usage}");
    }
    assert!(
        !usage.contains("M0"),
        "obsolete beta scope remained: {usage}"
    );
    assert!(!usage.contains("public beta"));
}

#[test]
fn channel_add_and_remove_update_the_ledger_and_default_removal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    let out = run(&root, &["add", "web"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    assert_eq!(value["result"]["channel"], "web");
    assert_eq!(value["result"]["added"], true);
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(state["channels"]["web"]["added_at"].is_number());

    let out = run(&root, &["remove", "web"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    assert_eq!(value["result"]["removed"], true);
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(state["removed_defaults"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("web")));
    assert!(state["channels"].get("web").is_none());

    assert!(run(&root, &["add", "web"]).status.success());
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(state["removed_defaults"].as_array().unwrap().is_empty());
}

#[test]
fn skill_link_status_unlink_work_through_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    let skills = dir.path().join("agent-skills");
    let out = run(&root, &["skill", "link", &skills.to_string_lossy()]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    let installed = skills.join("arana/SKILL.md");
    assert!(installed.is_file());
    let out = run(&root, &["skill", "status"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["result"]["installed"], true);
    let out = run(&root, &["skill", "unlink", &skills.to_string_lossy()]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    assert!(!installed.exists());
}

#[test]
fn uninstall_previews_by_default_and_keeps_unowned_files_after_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    assert!(run(&root, &["add", "web"]).status.success());
    std::fs::write(root.join("mine.txt"), "keep").unwrap();
    let out = run(&root, &["uninstall"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    assert_eq!(value["result"]["dry_run"], true);
    assert_eq!(value["result"]["confirm"], "arana uninstall --yes");
    assert!(root.join("mine.txt").is_file());
    assert!(root.join("state.json").is_file());
    assert!(run(&root, &["uninstall", "--yes"]).status.success());
    assert!(root.join("mine.txt").is_file());
    assert!(!root.join("state.json").exists());
}

#[test]
fn update_check_is_honest_and_does_not_create_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("absent");
    let out = run(&root, &["update", "--check"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{value}");
    assert_eq!(value["result"]["release_check"], "unavailable");
    assert_eq!(value["result"]["update_available"], false);
    assert!(!root.exists());
}

#[test]
fn channels_without_login_state_reject_login_and_logout() {
    let dir = tempfile::tempdir().unwrap();
    for command in ["login", "logout"] {
        let out = run(dir.path(), &[command, "web"]);
        assert!(!out.status.success());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["error"]["code"], "BAD_INPUT");
    }
}

#[test]
fn doctor_does_not_trust_stale_or_unverified_check_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::json!({
            "checks": {"web": {
                "at": now,
                "actions": {"search": true},
                "previous": {},
                "codes": {},
                "evidence": {"search": {"at": now, "verified": true, "fingerprint": "stale"}},
                "scope": "all"
            }}
        })
        .to_string(),
    )
    .unwrap();
    let out = run(dir.path(), &["doctor"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success());
    let web = value["result"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["channel"] == "web")
        .unwrap();
    assert_eq!(web["status"], "unverified");
    assert_eq!(web["verified_actions"].as_array().unwrap().len(), 0);
}

#[test]
fn setup_requires_an_existing_legacy_directory_before_any_install() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("absent");
    let missing = dir.path().join("no-legacy");
    let out = run(
        &root,
        &["setup", "--migrate", "--legacy", &missing.to_string_lossy()],
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!out.status.success());
    assert_eq!(value["error"]["code"], "NEED_SETUP");
    assert!(
        !root.exists(),
        "setup created state before validating legacy"
    );
}

#[test]
fn check_text_renders_human_lines_and_never_mixes_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &["check", "xiaoyuzhou", "--timeout", "1", "--text"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(text.contains("FAIL  xiaoyuzhou"), "{text}");
    assert!(text.contains("actions working"), "{text}");
    assert!(!text.trim_start().starts_with('{'), "{text}");
}
