//! The recorded goldens and `schema.json` are the frozen contract for
//! schema_version 1. These tests fail if a recorded envelope (or the schema that
//! describes it) stops matching the rules the CLI promises.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}

fn read_json(path: &PathBuf) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

const ENVELOPE_KEYS: [&str; 8] = [
    "schema_version",
    "ok",
    "channel",
    "action",
    "backend",
    "items",
    "meta",
    "error",
];

const ITEM_KEYS: [&str; 6] = ["title", "url", "author", "published_at", "text", "raw"];

const CODES: [&str; 9] = [
    "NEED_SETUP",
    "NEED_LOGIN",
    "UPSTREAM_BROKEN",
    "TIMEOUT",
    "RATE_LIMITED",
    "PLATFORM_BLOCKED",
    "NOT_FOUND",
    "BAD_INPUT",
    "BUSY",
];

fn keys(value: &Value, context: &str) -> Vec<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{context} must be an object"))
        .keys()
        .cloned()
        .collect()
}

/// Every rule in `schema.json`, enforced directly (no schema engine needed).
fn validate_envelope(envelope: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if !envelope.is_object() {
        return vec!["envelope is not an object".into()];
    }
    let actual = keys(envelope, "envelope");
    if actual != ENVELOPE_KEYS {
        problems.push(format!("envelope keys are {actual:?}"));
    }
    if envelope["schema_version"] != 1 {
        problems.push("schema_version is not 1".into());
    }
    let ok = match envelope["ok"].as_bool() {
        Some(value) => value,
        None => {
            problems.push("ok is not a boolean".into());
            return problems;
        }
    };
    for key in ["channel", "action", "backend"] {
        if !(envelope[key].is_null() || envelope[key].is_string()) {
            problems.push(format!("{key} is neither string nor null"));
        }
    }
    let items = match envelope["items"].as_array() {
        Some(items) => items,
        None => {
            problems.push("items is not an array".into());
            return problems;
        }
    };
    for (index, item) in items.iter().enumerate() {
        let item_keys = keys(item, "item");
        if item_keys != ITEM_KEYS {
            problems.push(format!("items[{index}] keys are {item_keys:?}"));
        }
        for key in ["title", "url", "author", "published_at", "text"] {
            if !(item[key].is_null() || item[key].is_string()) {
                problems.push(format!("items[{index}].{key} is neither string nor null"));
            }
        }
    }
    let meta = &envelope["meta"];
    if !meta.is_object() {
        problems.push("meta is not an object".into());
    } else {
        if meta.get("elapsed_ms").and_then(Value::as_u64).is_none() {
            problems.push("meta.elapsed_ms is not a non-negative integer".into());
        }
        match meta.get("attempts").and_then(Value::as_array) {
            Some(attempts) => {
                for (index, attempt) in attempts.iter().enumerate() {
                    if !attempt.is_object() {
                        problems.push(format!("attempts[{index}] is not an object"));
                        continue;
                    }
                    if attempt.get("backend").and_then(Value::as_str).is_none() {
                        problems.push(format!("attempts[{index}].backend is not a string"));
                    }
                    match attempt.get("result").and_then(Value::as_str) {
                        Some("ok") | Some("INSUFFICIENT_CONTENT") => {}
                        Some(code) if CODES.contains(&code) => {}
                        other => problems.push(format!("attempts[{index}].result is {other:?}")),
                    }
                    for extra in ["message", "retry_after_seconds", "skipped"] {
                        if let Some(value) = attempt.get(extra) {
                            let valid = match extra {
                                "message" => value.is_string(),
                                "skipped" => value.is_boolean(),
                                "retry_after_seconds" => value.is_number(),
                                _ => unreachable!(),
                            };
                            if !valid {
                                problems.push(format!("attempts[{index}].{extra} has a bad type"));
                            }
                        }
                    }
                    let allowed: BTreeSet<&str> = [
                        "backend",
                        "result",
                        "message",
                        "retry_after_seconds",
                        "skipped",
                    ]
                    .into_iter()
                    .collect();
                    let seen: BTreeSet<&str> = attempt
                        .as_object()
                        .map(|object| object.keys().map(String::as_str).collect())
                        .unwrap_or_default();
                    if !seen.is_subset(&allowed) {
                        problems.push(format!("attempts[{index}] has unexpected keys {seen:?}"));
                    }
                }
            }
            None => problems.push("meta.attempts is not an array".into()),
        }
        if let Some(access) = meta.get("access") {
            if access.get("mode").and_then(Value::as_str) != Some("public") {
                problems.push("meta.access.mode is not public".into());
            }
            if access.get("completeness_verified").and_then(Value::as_bool) != Some(false) {
                problems.push("meta.access.completeness_verified is not false".into());
            }
            match access.get("scope").and_then(Value::as_str) {
                Some("search_results") | Some("page_text") => {}
                other => problems.push(format!("meta.access.scope is {other:?}")),
            }
        }
    }
    let error = &envelope["error"];
    if ok {
        if !error.is_null() {
            problems.push("ok is true but error is set".into());
        }
    } else {
        if !envelope["backend"].is_null() {
            problems.push("ok is false but backend is set".into());
        }
        if !items.is_empty() {
            problems.push("ok is false but items is not empty".into());
        }
        if !error.is_object() {
            problems.push("ok is false but error is not an object".into());
        } else {
            let error_keys = keys(error, "error");
            if error_keys != ["code", "message", "hint"] {
                problems.push(format!("error keys are {error_keys:?}"));
            }
            match error.get("code").and_then(Value::as_str) {
                Some(code) if CODES.contains(&code) => {}
                other => problems.push(format!("error.code is {other:?}")),
            }
            match error.get("message").and_then(Value::as_str) {
                Some(message) if message.chars().count() <= 500 => {}
                Some(message) => problems.push(format!(
                    "error.message is {} chars (max 500)",
                    message.chars().count()
                )),
                None => problems.push("error.message is not a string".into()),
            }
            if error.get("hint").and_then(Value::as_str).is_none() {
                problems.push("error.hint is not a string".into());
            }
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if message.contains("://") && message.contains('@') && !message.contains("[redacted]") {
                problems.push("error.message may carry URL credentials".into());
            }
        }
    }
    problems
}

fn records() -> Vec<Value> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .expect("golden dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".json") && name.contains("__"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    paths.iter().map(read_json).collect()
}

#[test]
fn every_declared_action_has_a_recording() {
    let records = records();
    let manifest = read_json(&golden_dir().join("manifest.json"));
    assert_eq!(manifest["action_count"], 40);
    assert_eq!(manifest["channel_count"], 14);
    assert_eq!(records.len(), 40, "one record per action");

    let recorded: BTreeSet<String> = records
        .iter()
        .map(|record| {
            format!(
                "{}:{}",
                record["channel"].as_str().unwrap_or_default(),
                record["action"].as_str().unwrap_or_default()
            )
        })
        .collect();
    let declared: BTreeSet<String> = reach_channels::registry()
        .iter()
        .flat_map(|channel| {
            channel
                .actions
                .iter()
                .map(move |action| format!("{}:{}", channel.name, action.name))
        })
        .collect();
    assert_eq!(
        recorded, declared,
        "recording set and registry set must match"
    );

    for record in &records {
        let name = format!(
            "{}:{}",
            record["channel"].as_str().unwrap_or_default(),
            record["action"].as_str().unwrap_or_default()
        );
        assert!(
            record["params"].is_object(),
            "{name} must record the params it used"
        );
        assert!(
            record.get("envelope").is_some(),
            "{name} must carry an `envelope` field (null when no envelope was produced)"
        );
        if record["envelope"].is_null() {
            assert_eq!(
                record["timed_out"], true,
                "{name} produced no envelope without reporting a timeout"
            );
            assert!(
                record["live_success"] == false,
                "{name} cannot claim a live success without an envelope"
            );
        }
    }
}

#[test]
fn recorded_envelopes_obey_schema_version_1() {
    for record in records() {
        let envelope = &record["envelope"];
        if envelope.is_null() {
            continue;
        }
        let name = format!(
            "{}:{}",
            record["channel"].as_str().unwrap_or_default(),
            record["action"].as_str().unwrap_or_default()
        );
        let problems = validate_envelope(envelope);
        assert!(
            problems.is_empty(),
            "{name} violates the contract: {problems:?}"
        );
        assert_eq!(
            envelope["channel"], record["channel"],
            "{name} channel mismatch"
        );
        assert_eq!(
            envelope["action"], record["action"],
            "{name} action mismatch"
        );
        let declared = reach_channels::channel(record["channel"].as_str().unwrap_or_default())
            .and_then(|channel| channel.action(record["action"].as_str().unwrap_or_default()))
            .expect("recorded action exists in the registry");
        if envelope["ok"].as_bool() == Some(true) {
            let labels: Vec<&str> = declared
                .backends
                .iter()
                .map(|(label, _)| label.as_str())
                .collect();
            let backend = envelope["backend"].as_str().unwrap_or_default();
            assert!(
                labels.contains(&backend) || backend == "public-web",
                "{name} reports backend {backend:?}, which the registry does not declare ({labels:?})"
            );
        }
    }
}

#[test]
fn schema_json_describes_the_same_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let schema = read_json(&root.join("schema.json"));
    assert_eq!(
        schema["$defs"]["item"]["required"],
        serde_json::json!(ITEM_KEYS)
    );
    assert_eq!(
        schema["required"],
        serde_json::json!(ENVELOPE_KEYS),
        "schema.json top-level required list drifted"
    );
    let codes = schema["$defs"]["error"]["properties"]["code"]["enum"]
        .as_array()
        .expect("code enum");
    let codes: Vec<&str> = codes.iter().filter_map(Value::as_str).collect();
    assert_eq!(codes, CODES);
    assert_eq!(
        schema["$defs"]["meta"]["required"],
        serde_json::json!(["elapsed_ms", "attempts"])
    );
    assert_eq!(
        schema["$defs"]["error"]["required"],
        serde_json::json!(["code", "message", "hint"])
    );
    assert_eq!(
        schema["$defs"]["error"]["properties"]["message"]["maxLength"],
        500
    );
}

#[test]
fn paused_attempt_fields_are_validated_by_their_declared_types() {
    let mut out = reach_core::envelope(
        Some("web"),
        Some("read"),
        None,
        vec![],
        0,
        vec![
            serde_json::json!({"backend":"jina","result":"TIMEOUT","skipped":true,"retry_after_seconds":10,"message":"paused"}),
        ],
        Some(reach_core::ReachError::new("TIMEOUT", "paused")),
    );
    assert!(validate_envelope(&out).is_empty());
    for (key, value) in [
        ("skipped", serde_json::json!("true")),
        ("retry_after_seconds", serde_json::json!("10")),
        ("message", serde_json::json!(10)),
    ] {
        let original = out["meta"]["attempts"][0][key].clone();
        out["meta"]["attempts"][0][key] = value;
        assert!(
            !validate_envelope(&out).is_empty(),
            "{key} must reject the wrong type"
        );
        out["meta"]["attempts"][0][key] = original;
    }
}

#[test]
fn recorded_items_have_no_duplicated_fields() {
    for record in records() {
        let envelope = &record["envelope"];
        if envelope.is_null() {
            continue;
        }
        for item in envelope["items"].as_array().expect("items") {
            let object: &Map<String, Value> = item.as_object().expect("item object");
            assert_eq!(object.len(), 6);
            assert!(object.contains_key("raw"));
        }
    }
}
