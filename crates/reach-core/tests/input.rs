use reach_core::input::{parse, validate};
use serde_json::json;
fn action() -> serde_json::Value {
    json!({"name":"search","params":[{"name":"query","type":"str","required":true,"default":null},{"name":"limit","type":"int","required":false,"default":5}],"backends":["exa"],"timeout":null})
}
fn strings(a: &[&str]) -> Vec<String> {
    a.iter().map(|v| v.to_string()).collect()
}
#[test]
fn params_validate_before_execution() {
    assert_eq!(
        validate(&action(), &json!({"query":"  rust ","limit":"2"}), 30.0).unwrap(),
        json!({"query":"rust","limit":2})
    );
    for v in [
        json!({}),
        json!({"query":""}),
        json!({"query":true}),
        json!({"query":"x","limit":0}),
        json!({"query":"x","limit":true}),
        json!({"query":"x","limit":1.5}),
        json!({"query":"x","extra":1}),
    ] {
        assert_eq!(validate(&action(), &v, 30.0).unwrap_err().code, "BAD_INPUT");
    }
    for t in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        assert!(validate(&action(), &json!({"query":"x"}), t).is_err());
    }
}
#[test]
fn literal_separator_preserves_flag_looking_values() {
    let p = parse(
        &action(),
        &strings(&["--limit", "2", "--", "--timeout", "--verbose"]),
    )
    .unwrap();
    assert_eq!(p.params["query"], "--timeout --verbose");
    assert_eq!(p.params["limit"], 2);
    assert_eq!(p.timeout, 30.0);
    assert!(!p.verbose);
    let p = parse(
        &action(),
        &strings(&[
            "rust",
            "programming",
            "--timeout",
            "2",
            "--backend",
            "exa",
            "--verbose",
        ]),
    )
    .unwrap();
    assert_eq!(p.params["query"], "rust programming");
    assert_eq!(p.timeout, 2.0);
    assert!(p.verbose);
    assert!(parse(&action(), &strings(&["x", "--unknown", "v"])).is_err());
}
