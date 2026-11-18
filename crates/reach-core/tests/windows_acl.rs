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
    // Inspect the binary ACL: SDDL can abbreviate a real SID as `LA`.
    let acl = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", r#"
            $ErrorActionPreference = 'Stop'
            $acl = [System.IO.File]::GetAccessControl($env:ARANA_ACL_TEST_FILE)
            $raw = [System.Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
            $acl.AreAccessRulesProtected
            foreach ($ace in $raw.DiscretionaryAcl) {
                '{0} {1:X} {2}' -f $ace.AceType, $ace.AccessMask, $ace.SecurityIdentifier.Value
            }
        "#])
        .env("ARANA_ACL_TEST_FILE", d.path().join("state.json"))
        .output()
        .unwrap();
    assert!(
        acl.status.success(),
        "{}",
        String::from_utf8_lossy(&acl.stderr)
    );
    assert_eq!(
        String::from_utf8(acl.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["True", &format!("AccessAllowed 1F01FF {sid}")],
        "the protected ACL must grant full access to exactly the token SID"
    );
}
