use reach_core::{envelope, make_item, ReachError};
use serde_json::{json, Value};

#[test]
fn envelope_contract_has_null_fields_and_final_errors() {
    let item = make_item(None, None, None, None, None, Value::Null);
    assert_eq!(
        item,
        json!({"title":null,"url":null,"author":null,"published_at":null,"text":null,"raw":null})
    );
    let error = ReachError::new("NEED_LOGIN", "user must log in");
    assert!(error.is_final());
    assert!(error.hint.contains("never ask for a password"));
    let out = envelope(
        Some("youtube"),
        Some("read"),
        Some("yt-dlp"),
        vec![item],
        3,
        vec![],
        Some(error),
    );
    assert_eq!(out["schema_version"], 1);
    assert_eq!(out["ok"], false);
    assert_eq!(out["backend"], Value::Null);
    assert_eq!(out["items"], json!([]));
    assert_eq!(out["meta"], json!({"elapsed_ms":3,"attempts":[]}));
}

#[test]
fn errors_scrub_credentials_queries_and_bound_unicode() {
    let e = ReachError::new(
        "UPSTREAM_BROKEN",
        format!(
            "https://user:password@example.com/path?api_key=secret&safe=yes {}",
            "界".repeat(600)
        ),
    );
    assert!(!e.message.contains("password"));
    assert!(!e.message.contains("secret"));
    assert!(e.message.chars().count() <= 500);
    assert!(e.message.contains("safe=yes"));
    assert!(!ReachError::new("NEED_SETUP", "missing").is_final());
}
