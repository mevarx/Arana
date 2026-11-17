use reach_channels::session;
use reach_core::state::Store;
use serde_json::json;
use std::{fs, path::Path};
#[test]
fn session_ids_cannot_escape_the_private_session_tree() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "keep").unwrap();
    Store::new(&root)
        .mutate(|s| {
            s["owned_paths"]["sessions/../../outside"] =
                json!({"owner":"arana","scope":"session","kind":"directory"})
        })
        .unwrap();
    assert!(session::clear_session("../../outside", &root).is_err());
    assert!(outside.join("keep").is_file());
    for name in [
        "../other",
        "../../outside",
        "/absolute",
        "C:\\outside",
        "nested/name",
        "",
    ] {
        assert!(
            session::session_dir(name, &root).is_err(),
            "invalid session id accepted: {name}"
        );
    }
}
#[test]
fn installed_tool_does_not_own_preexisting_unrecorded_session_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let dir = root.join("sessions/bili-cli");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("user-file"), "keep").unwrap();
    Store::new(root)
        .mutate(|s| s["tools"]["bili-cli"] = json!({"owner":"arana"}))
        .unwrap();
    assert!(session::session_dir("bili-cli", root).is_err());
    assert!(dir.join("user-file").exists());
}
#[test]
fn malformed_credentials_and_arbitrary_files_are_unknown_not_logged_in_or_out() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp
        .path()
        .join("sessions/bili-cli/.bilibili-cli/credential.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "malformed").unwrap();
    assert!(session::has_login_state("bili-cli", temp.path()).is_err());
    fs::create_dir_all(temp.path().join("sessions/boss-agent-cli")).unwrap();
    fs::write(
        temp.path().join("sessions/boss-agent-cli/config.json"),
        "{}",
    )
    .unwrap();
    assert!(session::has_login_state("boss-agent-cli", temp.path()).is_err());
}
#[cfg(windows)]
fn link_dir(source: &Path, target: &Path) {
    let status = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(target.to_string_lossy().replace('/', "\\"))
        .arg(source.to_string_lossy().replace('/', "\\"))
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "junction fixture failed: {} {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
}
#[cfg(unix)]
fn link_dir(source: &Path, target: &Path) {
    std::os::unix::fs::symlink(source, target).unwrap();
}
#[test]
fn credential_reads_never_follow_a_linked_session_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("credential.json"),
        r#"{"sessdata":"fixture-not-a-real-cookie"}"#,
    )
    .unwrap();
    let session = root.join("sessions/bili-cli");
    fs::create_dir_all(&session).unwrap();
    link_dir(&outside, &session.join(".bilibili-cli"));
    assert!(session::has_login_state("bili-cli", &root).is_err());
}
