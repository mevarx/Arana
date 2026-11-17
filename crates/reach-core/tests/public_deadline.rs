use async_trait::async_trait;
use reach_core::{
    clock::Clock,
    dispatch::{pace, run, Backend, PublicRoute, Request},
    state::Store,
    ReachError, Result,
};
use serde_json::{json, Value};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
struct Script {
    calls: Mutex<Vec<String>>,
    answers: Mutex<Vec<Result<Vec<Value>>>>,
}
#[async_trait]
impl Backend for Script {
    async fn call(
        &self,
        c: &str,
        _a: &str,
        b: &str,
        _p: &Value,
        _t: Duration,
    ) -> Result<Vec<Value>> {
        self.calls.lock().unwrap().push(format!("{c}/{b}"));
        self.answers.lock().unwrap().remove(0)
    }
}
fn script(a: Vec<Result<Vec<Value>>>) -> Script {
    Script {
        calls: Mutex::new(vec![]),
        answers: Mutex::new(a),
    }
}
fn req(added: bool) -> Request {
    Request {
        channel: "site".into(),
        action: "read".into(),
        backends: vec!["site-tool".into()],
        params: json!({"target":"https://example.com/article"}),
        timeout: Duration::from_secs(10),
        only_backend: None,
        public: Some(PublicRoute {
            action: "read".into(),
            params: json!({"target":"https://example.com/article"}),
            scopes: vec!["example.com".into()],
        }),
        added,
        paced: false,
        fingerprint: "1".into(),
    }
}
fn item() -> Value {
    json!({"title":"Article","url":"https://example.com/article","text":"Real readable article body.","raw":{"id":1}})
}
#[tokio::test]
async fn public_first_and_added_last_resort_preserve_provenance() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![Ok(vec![item()])]);
    let out = run(&b, &s, &req(false)).await;
    assert_eq!(out["backend"], "public-web");
    assert_eq!(out["meta"]["attempts"][0]["backend"], "public-web/obscura");
    assert_eq!(out["items"][0]["raw"]["completeness_verified"], false);
    let b = script(vec![
        Err(ReachError::new("UPSTREAM_BROKEN", "transient")),
        Ok(vec![item()]),
    ]);
    let out = run(&b, &s, &req(true)).await;
    assert_eq!(out["backend"], "public-web");
    assert_eq!(out["meta"]["attempts"].as_array().unwrap().len(), 2);
}
#[tokio::test]
async fn final_site_failure_prevents_public_fallback() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![Err(ReachError::new("NEED_LOGIN", "login"))]);
    let out = run(&b, &s, &req(true)).await;
    assert_eq!(out["error"]["code"], "NEED_LOGIN");
    assert_eq!(*b.calls.lock().unwrap(), vec!["site/site-tool"]);
}
#[tokio::test]
async fn public_scope_and_login_wall_checks_reject_unusable_content() {
    for bad in [
        json!({"title":"Sign in","url":"https://example.com/article","text":"Please sign in to continue"}),
        json!({"title":"Article","url":"https://example.com.evil.org/article","text":"outside scope"}),
    ] {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        let b = script(vec![Ok(vec![bad])]);
        let mut r = req(false);
        r.only_backend = Some("public-web".into());
        let out = run(&b, &s, &r).await;
        assert_eq!(out["error"]["code"], "UPSTREAM_BROKEN");
        assert_eq!(out["meta"]["attempts"][1]["result"], "INSUFFICIENT_CONTENT");
    }
}
#[tokio::test]
async fn public_provider_rate_limit_does_not_pause_destination() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let b = script(vec![
        Err(ReachError::new("RATE_LIMITED", "provider limit")),
        Ok(vec![item()]),
    ]);
    let out = run(&b, &s, &req(false)).await;
    assert_eq!(out["backend"], "site-tool");
    assert!(s.load().unwrap()["cooldowns"].get("site").is_none());
    assert_eq!(
        s.load().unwrap()["cooldowns"]["web"]["code"],
        "RATE_LIMITED"
    );
}
struct Slow;
#[async_trait]
impl Backend for Slow {
    async fn call(
        &self,
        _c: &str,
        _a: &str,
        _b: &str,
        _p: &Value,
        _t: Duration,
    ) -> Result<Vec<Value>> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn global_deadline_cancels_pending_backend() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let mut r = req(true);
    r.public = None;
    r.timeout = Duration::from_millis(30);
    let started = Instant::now();
    let out = run(&Slow, &s, &r).await;
    assert_eq!(out["error"]["code"], "TIMEOUT");
    assert!(started.elapsed() < Duration::from_secs(3));
}
#[tokio::test]
async fn pacing_is_serialized_and_short_budget_fails_before_call() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    pace(&s, "site", &Clock::new(Duration::from_secs(20)))
        .await
        .unwrap();
    let e = pace(&s, "site", &Clock::new(Duration::from_millis(100)))
        .await
        .unwrap_err();
    assert_eq!(e.code, "TIMEOUT");
    let start = Instant::now();
    pace(&s, "site", &Clock::new(Duration::from_secs(15)))
        .await
        .unwrap();
    assert!(start.elapsed() > Duration::from_secs(9));
}
