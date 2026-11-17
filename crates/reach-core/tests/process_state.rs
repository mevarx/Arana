use reach_core::state::{FileLock, Store};
use serde_json::json;
use std::{
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn child_updates_state() {
    let Ok(root) = std::env::var("ARANA_TEST_STATE_CHILD") else {
        return;
    };
    let store = Store::new(root);
    for _ in 0..8 {
        let mut done = false;
        for _ in 0..3000 {
            match store.mutate(|v| {
                let n = v["stress_counter"].as_u64().unwrap_or(0);
                v["stress_counter"] = json!(n + 1);
            }) {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) if e.code == "BUSY" => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("{e}"),
            }
        }
        assert!(done, "state writer starved");
    }
}
#[test]
fn processes_do_not_lose_atomic_state_updates() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    // Initialize private root once before concurrent children.
    Store::new(dir.path())
        .mutate(|v| v["stress_counter"] = json!(0))
        .unwrap();
    let mut children = vec![];
    for _ in 0..4 {
        children.push(
            Command::new(&exe)
                .args(["--exact", "child_updates_state", "--nocapture"])
                .env("ARANA_TEST_STATE_CHILD", dir.path())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(Store::new(dir.path()).load().unwrap()["stress_counter"], 32);
}
#[test]
fn release_of_os_lock_does_not_require_deleting_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    {
        let _lock = FileLock::try_acquire(dir.path(), "resource").unwrap();
        assert_eq!(
            FileLock::try_acquire(dir.path(), "resource")
                .err()
                .unwrap()
                .code,
            "BUSY"
        );
    }
    assert!(FileLock::try_acquire(dir.path(), "resource").is_ok());
}
