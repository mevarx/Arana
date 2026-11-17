use reach_core::{
    clock::Clock,
    state::{FileLock, Store},
};
use serde_json::json;
use std::time::Duration;

#[test]
fn corrupt_state_defaults_and_mutations_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    std::fs::write(dir.path().join("state.json"), "{invalid").unwrap();
    assert_eq!(store.load().unwrap()["version"], 1);
    store
        .mutate(|v| {
            v["channels"]["web"] = json!({"added_at":"test"});
        })
        .unwrap();
    assert_eq!(store.load().unwrap()["channels"]["web"]["added_at"], "test");
    let _held = FileLock::try_acquire(dir.path(), "reach-state-write").unwrap();
    assert_eq!(
        store.mutate(|v| v["version"] = json!(2)).unwrap_err().code,
        "BUSY"
    );
    assert_eq!(store.load().unwrap()["version"], 1);
}

#[test]
fn state_rejects_symlink_components() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), dir.path().join("redirect")).unwrap();
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(outside.path(), dir.path().join("redirect")).is_err() {
            return;
        }
    }
    let store = Store::new(dir.path().join("redirect"));
    assert!(store.mutate(|_| {}).is_err());
}

#[test]
fn fake_clock_pauses_off_clock_and_nested_pauses_balance() {
    let c = Clock::manual(Duration::from_secs(30));
    c.advance(Duration::from_secs(5));
    assert_eq!(c.remaining(), Duration::from_secs(25));
    {
        let _pause = c.pause();
        c.advance(Duration::from_secs(90));
        {
            let _nested = c.pause();
            c.advance(Duration::from_secs(20));
        }
        assert_eq!(c.remaining(), Duration::from_secs(25));
    }
    c.advance(Duration::from_secs(25));
    assert!(c.remaining().is_zero());
}
