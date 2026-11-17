use reach_browser::{cookie_domain_matches, filter_live_cookies, SlotConfig, Slots};
use serde_json::json;
use std::{
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn site_cookie_matching_requires_a_domain_boundary() {
    assert!(cookie_domain_matches(".a.reddit.com", "reddit.com"));
    assert!(cookie_domain_matches("reddit.com", ".REDDIT.COM"));
    assert!(!cookie_domain_matches("notreddit.com", "reddit.com"));
    assert!(!cookie_domain_matches("evilreddit.com", "reddit.com"));
}

#[test]
fn expired_cookie_is_filtered_but_session_and_future_cookies_survive() {
    let rows = vec![
        json!({"name":"dead", "domain":"example.com", "expires": 1}),
        json!({"name":"session", "domain":"example.com", "expires": -1}),
        json!({"name":"live", "domain":"example.com", "expires": 4_000_000_000i64}),
    ];
    let live = filter_live_cookies(rows, 2_000_000_000);
    assert_eq!(
        live.iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session", "live"]
    );
}

#[tokio::test]
async fn slots_enforce_site_and_global_caps_then_release() {
    let dir = tempfile::tempdir().unwrap();
    let config = SlotConfig {
        per_site: 2,
        global: 4,
        wait: Duration::from_millis(25),
        poll: Duration::from_millis(2),
    };
    let a = Slots::new(dir.path(), config);
    let held1 = a.acquire("reddit").await.unwrap();
    let held2 = a.acquire("reddit").await.unwrap();
    assert_eq!(a.acquire("reddit").await.unwrap_err().code, "BUSY");
    drop(held1);
    let held3 = a.acquire("reddit").await.unwrap();
    let b = Slots::new(
        dir.path(),
        SlotConfig {
            per_site: 2,
            global: 4,
            wait: Duration::from_millis(25),
            poll: Duration::from_millis(2),
        },
    );
    let _held4 = b.acquire("twitter").await.unwrap();
    drop((held2, held3));
}

#[tokio::test]
async fn global_slots_are_shared_across_site_names() {
    let dir = tempfile::tempdir().unwrap();
    let config = SlotConfig {
        per_site: 3,
        global: 2,
        wait: Duration::from_millis(20),
        poll: Duration::from_millis(2),
    };
    let slots = Slots::new(dir.path(), config);
    let a = slots.acquire("site-a").await.unwrap();
    let b = slots.acquire("site-b").await.unwrap();
    assert_eq!(slots.acquire("site-c").await.unwrap_err().code, "BUSY");
    drop((a, b));
}
#[tokio::test]
async fn total_four_slots_are_shared_between_sites_under_concurrent_acquisition() {
    let dir = tempfile::tempdir().unwrap();
    let config = SlotConfig {
        per_site: 2,
        global: 4,
        wait: Duration::from_millis(40),
        poll: Duration::from_millis(2),
    };
    let slots = Slots::new(dir.path(), config);
    let (a, b, c, d) = tokio::join!(
        slots.acquire("reddit"),
        slots.acquire("twitter"),
        slots.acquire("linkedin"),
        slots.acquire("facebook")
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok() && d.is_ok());
    let a = a.unwrap();
    let b = b.unwrap();
    let c = c.unwrap();
    let d = d.unwrap();
    assert_eq!(slots.acquire("site-five").await.unwrap_err().code, "BUSY");
    drop((a, b, c, d));
}

#[tokio::test]
async fn site_limit_is_two_even_with_global_capacity_available() {
    let dir = tempfile::tempdir().unwrap();
    let slots = Slots::new(
        dir.path(),
        SlotConfig {
            per_site: 2,
            global: 4,
            wait: Duration::from_millis(20),
            poll: Duration::from_millis(2),
        },
    );
    let a = slots.acquire("same").await.unwrap();
    let b = slots.acquire("same").await.unwrap();
    assert_eq!(slots.acquire("same").await.unwrap_err().code, "BUSY");
    drop((a, b));
}

#[test]
fn subprocess_slot_holder() {
    let Ok(root) = std::env::var("ARANA_SLOT_HOLDER_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let slots = Slots::new(
        &root,
        SlotConfig {
            per_site: 2,
            global: 1,
            wait: Duration::from_secs(2),
            poll: Duration::from_millis(2),
        },
    );
    let _guard = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(slots.acquire("subprocess"))
        .unwrap();
    std::fs::write(root.join("holder-ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_millis(800));
}

#[tokio::test]
async fn global_lock_is_enforced_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_slot_holder", "--nocapture"])
        .env("ARANA_SLOT_HOLDER_ROOT", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ready = dir.path().join("holder-ready");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let slots = Slots::new(
        dir.path(),
        SlotConfig {
            per_site: 2,
            global: 1,
            wait: Duration::from_millis(50),
            poll: Duration::from_millis(2),
        },
    );
    assert_eq!(
        slots.acquire("another-process").await.unwrap_err().code,
        "BUSY"
    );
    assert!(child.wait().unwrap().success());
}
