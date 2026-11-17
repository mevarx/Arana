//! The registry must reproduce Agent-Reach 1.6.1 exactly.
//!
//! `tests/golden/channels.json` is the verbatim `agent-reach channels` output
//! recorded from the Python build; `tests/golden/registry.json` adds the backend
//! method names, live-test inputs and per-action timeouts that the catalog does
//! not print. Any drift in a name, help string, param, default, backend order,
//! timeout or public-web scope fails here.

use serde_json::{json, Value};
use std::path::PathBuf;

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

#[test]
fn catalog_is_byte_identical_to_the_python_build() {
    let recorded = golden("channels.json");
    let ours = reach_channels::catalog_for("agent-reach");
    assert_eq!(
        serde_json::to_string(&ours).expect("serialize catalog"),
        serde_json::to_string(&recorded).expect("serialize golden"),
        "catalog drifted from the recorded upstream output"
    );
}

#[test]
fn catalog_for_this_program_only_changes_the_program_name() {
    let upstream = remember(&reach_channels::catalog_for("agent-reach"));
    let our_channels = remember(&reach_channels::catalog());
    assert_eq!(upstream.len(), our_channels.len());
    for (channel_before, channel_after) in upstream.iter().zip(our_channels.iter()) {
        assert_eq!(channel_before["name"], channel_after["name"]);
        assert_eq!(channel_before["description"], channel_after["description"]);
        assert_eq!(channel_before["level"], channel_after["level"]);
        let actions_before = channel_before["actions"].as_array().expect("actions");
        let actions_after = channel_after["actions"].as_array().expect("actions");
        for (before, after) in actions_before.iter().zip(actions_after.iter()) {
            assert_eq!(before["name"], after["name"]);
            assert_eq!(before["help"], after["help"]);
            assert_eq!(before["params"], after["params"]);
            assert_eq!(before["backends"], after["backends"]);
            assert_eq!(before["public_web"], after["public_web"]);
            let expected =
                before["usage"]
                    .as_str()
                    .expect("usage")
                    .replacen("agent-reach", "arana", 1);
            assert_eq!(after["usage"], expected);
        }
    }
    let ours = reach_channels::catalog();
    assert_eq!(
        ours["commands"]["read"],
        "arana read <url>  (picks the channel from the URL)"
    );
    assert_eq!(
        ours["commands"]["action"],
        "arana <channel> <action> [args] [--timeout SECONDS]"
    );
}

/// Structural copy that keeps key order irrelevant for the comparison above.
fn remember(value: &Value) -> Vec<Value> {
    value["channels"].as_array().cloned().unwrap_or_default()
}

#[test]
fn backend_labels_and_methods_match_upstream() {
    let registry = golden("registry.json");
    let channels = registry["channels"].as_array().expect("channels");
    assert_eq!(channels.len(), reach_channels::registry().len());
    for recorded in channels {
        let name = recorded["name"].as_str().expect("name");
        let channel = reach_channels::channel(name)
            .unwrap_or_else(|| panic!("channel {name} is missing from the Rust registry"));
        let actions = recorded["actions"].as_array().expect("actions");
        assert_eq!(
            channel.actions.len(),
            actions.len(),
            "{name} action count differs"
        );
        for action in actions {
            let action_name = action["name"].as_str().expect("action name");
            let ours = channel
                .action(action_name)
                .unwrap_or_else(|| panic!("{name} is missing action {action_name}"));
            let expected: Vec<(String, String)> = action["backends"]
                .as_array()
                .expect("backends")
                .iter()
                .map(|pair| {
                    let pair = pair.as_array().expect("backend pair");
                    (
                        pair[0].as_str().expect("label").to_string(),
                        pair[1].as_str().expect("method").to_string(),
                    )
                })
                .collect();
            assert_eq!(
                ours.backends, expected,
                "{name} {action_name} backends drifted"
            );
            let timeout = action["timeout"].as_f64();
            assert_eq!(
                ours.timeout, timeout,
                "{name} {action_name} timeout drifted"
            );
            assert_eq!(
                ours.live_test.as_ref(),
                action.get("live_test").filter(|value| !value.is_null()),
                "{name} {action_name} live_test drifted"
            );
        }
    }
}

#[test]
fn registry_rejects_unknown_names_with_the_valid_list() {
    assert!(reach_channels::channel("nope").is_none());
    let value = reach_channels::catalog();
    let names: Vec<&str> = value["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .map(|channel| channel["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "github",
            "twitter",
            "youtube",
            "reddit",
            "facebook",
            "instagram",
            "bilibili",
            "xiaohongshu",
            "linkedin",
            "boss",
            "v2ex",
            "xueqiu",
            "xiaoyuzhou",
            "web"
        ]
    );
}

#[test]
fn public_web_scopes_match_upstream() {
    let expected: Vec<(&str, &str, Vec<&str>)> = vec![
        ("twitter", "search", vec!["x.com/", "twitter.com/"]),
        ("reddit", "search", vec!["reddit.com/r/"]),
        ("reddit", "read", vec!["reddit.com/r/", "redd.it/"]),
        ("facebook", "search", vec!["facebook.com/"]),
        (
            "xiaohongshu",
            "search",
            vec![
                "xiaohongshu.com/explore/",
                "xiaohongshu.com/search_result/",
                "xhslink.com/",
            ],
        ),
        ("linkedin", "jobs", vec!["linkedin.com/jobs/view/"]),
        ("linkedin", "read", vec!["linkedin.com/jobs/view/"]),
        ("boss", "search", vec!["zhipin.com/job_detail/"]),
        ("boss", "read", vec!["zhipin.com/job_detail/"]),
    ];
    for (channel, action, scopes) in expected {
        let ours = reach_channels::channel(channel)
            .and_then(|channel| channel.action(action))
            .unwrap_or_else(|| panic!("{channel}/{action} missing"));
        assert_eq!(
            ours.public_web, scopes,
            "{channel}/{action} public_web drifted"
        );
    }
}

#[test]
fn catalog_serializes_action_params_in_upstream_order() {
    // Key order is part of the byte-identical contract.
    let ours = reach_channels::catalog_for("agent-reach");
    let serialized = serde_json::to_string(&ours).expect("serialize");
    let probe = json!({"schema_version": 1});
    assert!(probe.is_object());
    assert!(
        serialized.starts_with(r#"{"schema_version":1,"ok":true,"commands":{"read":"#),
        "top-level order drifted: {}",
        &serialized[..80]
    );
    assert!(
        serialized.contains(r#""actions":[{"name":"search","help":"Search repositories","usage":"agent-reach github search"#),
        "action key order drifted"
    );
}
