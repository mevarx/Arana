use std::process::Command;
#[test]
fn idle_watcher_entrypoint_does_not_emit_agent_json() {
    let d = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_arana"))
        .arg("__browser-watch")
        .arg(d.path())
        .env("ARANA_HOME", d.path())
        .env("ARANA_BROWSER_PORT", "49179")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}
