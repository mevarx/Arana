use async_trait::async_trait;
use reach_core::{
    dispatch::{run, Backend, Request},
    state::Store,
    ReachError, Result,
};
use serde_json::{json, Value};
use std::{sync::Mutex, time::Duration};
struct Script {
    calls: Mutex<Vec<String>>,
    answers: Mutex<Vec<Result<Vec<Value>>>>,
}
#[async_trait]
impl Backend for Script {
    async fn call(
        &self,
        _c: &str,
        _a: &str,
        b: &str,
        _p: &Value,
        _t: Duration,
    ) -> Result<Vec<Value>> {
        self.calls.lock().unwrap().push(b.into());
        self.answers.lock().unwrap().remove(0)
    }
}
fn req() -> Request {
    Request {
        channel: "test".into(),
        action: "read".into(),
        backends: vec!["first".into(), "second".into()],
        params: json!({}),
        timeout: Duration::from_secs(5),
        only_backend: None,
        public: None,
        added: true,
        paced: false,
        fingerprint: "build1".into(),
    }
}
fn script(a: Vec<Result<Vec<Value>>>) -> Script {
    Script {
        calls: Mutex::new(vec![]),
        answers: Mutex::new(a),
    }
}
#[tokio::test]
async fn retryable_falls_through_health_is_skipped_and_pin_bypasses() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![
        Err(ReachError::new("TIMEOUT", "first timed out")),
        Ok(vec![json!({"text":"second"})]),
    ]);
    let out = run(&b, &s, &req()).await;
    assert_eq!(out["backend"], "second");
    assert_eq!(out["meta"]["attempts"].as_array().unwrap().len(), 2);
    let b = script(vec![Ok(vec![])]);
    let out = run(&b, &s, &req()).await;
    assert_eq!(out["meta"]["attempts"][0]["skipped"], true);
    assert_eq!(*b.calls.lock().unwrap(), vec!["second"]);
    let b = script(vec![Ok(vec![])]);
    let mut r = req();
    r.only_backend = Some("first".into());
    let out = run(&b, &s, &r).await;
    assert_eq!(out["backend"], "first");
    assert!(s.load().unwrap()["backend_health"]["test"]["read"]
        .get("first")
        .is_none());
}
#[tokio::test]
async fn final_stops_fallback_and_rate_limit_is_channel_wide() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![Err(ReachError::new("RATE_LIMITED", "HTTP429"))]);
    let out = run(&b, &s, &req()).await;
    assert_eq!(out["error"]["code"], "RATE_LIMITED");
    assert_eq!(b.calls.lock().unwrap().len(), 1);
    let b = script(vec![]);
    let mut r = req();
    r.action = "search".into();
    let out = run(&b, &s, &r).await;
    assert_eq!(out["error"]["code"], "RATE_LIMITED");
    assert!(b.calls.lock().unwrap().is_empty());
    assert!(out["error"]["hint"].as_str().unwrap().contains("seconds"));
}
#[tokio::test]
async fn real_failure_beats_missing_setup_and_fingerprint_repairs_pause() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![
        Err(ReachError::new("NEED_SETUP", "missing")),
        Err(ReachError::new("UPSTREAM_BROKEN", "real")),
    ]);
    assert_eq!(
        run(&b, &s, &req()).await["error"]["code"],
        "UPSTREAM_BROKEN"
    );
    let mut r = req();
    r.fingerprint = "repaired".into();
    let b = script(vec![Ok(vec![])]);
    assert_eq!(run(&b, &s, &r).await["backend"], "first");
}
