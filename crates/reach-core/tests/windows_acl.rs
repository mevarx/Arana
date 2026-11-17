#![cfg(windows)]
use reach_core::state::Store;
use serde_json::json;
use std::process::Command;
#[test]
fn child_writes_with_spoofed_username() {
    let Ok(root) = std::env::var("ARANA_ACL_TEST_CHILD") else {
        return;
    };
    Store::new(root)
        .mutate(|v| v["test"] = json!("private"))
        .expect("permissions must use process token, not USERNAME");
}
#[test]
fn spoofed_username_cannot_change_private_state_owner() {
    let d = tempfile::tempdir().unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_writes_with_spoofed_username",
            "--nocapture",
        ])
        .env("ARANA_ACL_TEST_CHILD", d.path())
        .env("USERNAME", "ARANA_NONEXISTENT_SPOOFED_ACCOUNT")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(Store::new(d.path()).load().unwrap()["test"], "private");
    let acl = Command::new("icacls.exe")
        .arg(d.path().join("state.json"))
        .output()
        .unwrap();
    assert!(acl.status.success());
    assert!(!String::from_utf8_lossy(&acl.stdout).contains("SPOOFED"));
    let identity = Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    assert!(identity.status.success());
    let who = String::from_utf8(identity.stdout).unwrap();
    let sid = who.trim().split(',').nth(1).unwrap().trim_matches('"');
    assert!(sid.starts_with("S-1-"));
    let dump = d.path().join("acl-sddl.txt");
    let saved = Command::new("icacls.exe")
        .arg(d.path().join("state.json"))
        .arg("/save")
        .arg(&dump)
        .output()
        .unwrap();
    assert!(saved.status.success());
    let raw = std::fs::read(dump).unwrap();
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    let sddl = String::from_utf16(&units).unwrap();
    assert!(
        sddl.contains(&format!("(A;;FA;;;{sid})")),
        "actual token SID must own the only grant: {sddl}"
    );
    assert_eq!(
        sddl.matches("(A;").count(),
        1,
        "there must be exactly one allow ACE: {sddl}"
    );
}
