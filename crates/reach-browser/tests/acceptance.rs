use reach_browser::{SlotConfig, Slots};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "Real default browser queue wait takes 120 seconds"]
async fn default_slot_exhaustion_returns_busy_after_120_seconds() {
    let root = tempfile::tempdir().unwrap();
    let slots = Slots::new(root.path(), SlotConfig::default());
    let a1 = slots.acquire("a.example").await.unwrap();
    let a2 = slots.acquire("a.example").await.unwrap();
    let b1 = slots.acquire("b.example").await.unwrap();
    let b2 = slots.acquire("b.example").await.unwrap();
    let started = Instant::now();
    let (same, new_site) = tokio::join!(slots.acquire("a.example"), slots.acquire("c.example"));
    let elapsed = started.elapsed();
    assert_eq!(same.unwrap_err().code, "BUSY");
    assert_eq!(new_site.unwrap_err().code, "BUSY");
    assert!(elapsed >= Duration::from_secs(120));
    assert!(elapsed < Duration::from_secs(135));
    println!("{{\"default_queue_seconds\":120,\"measured_seconds\":{:.3},\"same_site_code\":\"BUSY\",\"new_site_code\":\"BUSY\"}}",elapsed.as_secs_f64());
    drop((a1, a2, b1, b2));
    assert!(slots.acquire("a.example").await.is_ok());
}
