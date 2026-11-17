//! Fail-closed structural validation for operator-trusted release acceptance
//! manifests. Source/freshness checks do not cryptographically attest that
//! external network probes ran; use the real CLI recorder and review provenance.
use crate::{ReachError, Result};
use chrono::DateTime;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;
const MIN_PASSES: usize = 34;
const FROZEN_REQUIRED: [(&str, &str); 40] = [
    ("web", "search"),
    ("web", "read"),
    ("v2ex", "hot"),
    ("v2ex", "node"),
    ("v2ex", "read"),
    ("v2ex", "user"),
    ("github", "search"),
    ("github", "read"),
    ("bilibili", "search"),
    ("bilibili", "read"),
    ("bilibili", "hot"),
    ("bilibili", "subtitle"),
    ("youtube", "search"),
    ("youtube", "read"),
    ("youtube", "subtitle"),
    ("youtube", "transcribe"),
    ("xueqiu", "quote"),
    ("xueqiu", "search"),
    ("xueqiu", "hot_posts"),
    ("xueqiu", "hot_stocks"),
    ("xiaohongshu", "search"),
    ("xiaohongshu", "read"),
    ("xiaohongshu", "comments"),
    ("boss", "search"),
    ("boss", "read"),
    ("xiaoyuzhou", "transcribe"),
    ("linkedin", "jobs"),
    ("linkedin", "read"),
    ("reddit", "search"),
    ("reddit", "read"),
    ("reddit", "subreddit"),
    ("twitter", "search"),
    ("twitter", "profile"),
    ("twitter", "tweets"),
    ("twitter", "thread"),
    ("facebook", "search"),
    ("facebook", "profile"),
    ("instagram", "search"),
    ("instagram", "profile"),
    ("instagram", "posts"),
];

/// Validate the frozen 40-action release contract. `required` is the catalog
/// from the binary/manifest; source_digest must be the binary's build signature.
pub fn validate(
    manifest: &Value,
    required: &[(String, String)],
    source_digest: &str,
    now_seconds: i64,
) -> Result<Value> {
    let mut errors = Vec::<String>::new();
    let required_set: BTreeSet<_> = required.iter().cloned().collect();
    if required.len() != 40 || required_set.len() != 40 {
        errors.push("required action catalog must contain exactly 40 unique pairs".into());
    }
    let frozen: BTreeSet<_> = FROZEN_REQUIRED
        .iter()
        .map(|(c, a)| (c.to_string(), a.to_string()))
        .collect();
    if required_set != frozen {
        errors
            .push("required action catalog differs from the frozen upstream 1.6.1 contract".into());
    }
    if source_digest.len() != 64 || !source_digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        errors.push("current source digest must be 64 hexadecimal characters".into());
    }
    let Some(object) = manifest.as_object() else {
        return Err(ReachError::new(
            "BAD_INPUT",
            "acceptance manifest must be an object",
        ));
    };
    if object.get("source_sha256").and_then(Value::as_str) != Some(source_digest) {
        errors.push("manifest source digest does not match current binary".into());
    }
    if object.get("release_ready") != Some(&Value::Bool(true)) {
        errors.push("manifest release_ready must be true".into());
    }
    for field in ["pending", "additional_required_validation"] {
        if object
            .get(field)
            .is_some_and(|v| !v.is_null() && v != &json!([]) && v != &json!(false))
        {
            errors.push(format!("manifest has pending {field}"));
        }
    }
    let results = object
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| ReachError::new("BAD_INPUT", "acceptance results must be an array"))?;
    let waivers = object
        .get("waived")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let allowed = allowed_waivers();
    let mut waived = BTreeSet::new();
    for entry in waivers {
        let key = pair(&entry);
        let reason = entry
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        match key {
            Some(key) if required_set.contains(&key) => {
                if !waived.insert(key.clone()) {
                    errors.push(format!("duplicate waiver: {}/{}", key.0, key.1));
                }
                if allowed.get(&key).map(String::as_str) != Some(reason) {
                    errors.push(format!(
                        "waiver not in approved upstream policy: {}/{}",
                        key.0, key.1
                    ));
                }
            }
            _ => errors.push("waiver references an unknown action".into()),
        }
    }
    for key in allowed.keys() {
        if !required_set.contains(key) {
            errors.push(format!(
                "required catalog missing approved waiver action {}/{}",
                key.0, key.1
            ));
        }
    }
    if waived != allowed.keys().cloned().collect() {
        errors.push("waiver set must equal the six approved upstream waivers".into());
    }

    let mut seen = BTreeSet::new();
    let mut passed = BTreeSet::new();
    let mut failed = 0usize;
    for row in results {
        let Some(key) = pair(row) else {
            errors.push("result row must identify channel and action".into());
            continue;
        };
        if !required_set.contains(&key) {
            errors.push("unknown result action in manifest".into());
            continue;
        }
        if !seen.insert(key.clone()) {
            errors.push(format!("duplicate result: {}/{}", key.0, key.1));
            continue;
        }
        if waived.contains(&key) {
            errors.push(format!(
                "waived action also has a result: {}/{}",
                key.0, key.1
            ));
        }
        let is_pass = row.get("passed") == Some(&Value::Bool(true));
        let error_reason = row
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !is_pass {
            failed += 1;
            if error_reason.is_empty() {
                errors.push(format!(
                    "failed result needs an explicit reason: {}/{}",
                    key.0, key.1
                ));
            }
            continue;
        }
        if row.get("method").and_then(Value::as_str) != Some("real-cli") {
            errors.push(format!("result is not real-cli: {}/{}", key.0, key.1));
        }
        if row
            .get("backend")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!("result backend missing: {}/{}", key.0, key.1));
        }
        if row
            .get("os")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!("result OS missing: {}/{}", key.0, key.1));
        }
        if row.get("source_sha256").and_then(Value::as_str) != Some(source_digest) {
            errors.push(format!(
                "result source digest mismatch: {}/{}",
                key.0, key.1
            ));
        }
        if !row
            .get("item_count")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
        {
            errors.push(format!(
                "result must have positive item_count: {}/{}",
                key.0, key.1
            ));
        }
        let timestamp = row
            .get("at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        match timestamp {
            Some(at) => {
                let delta = now_seconds.saturating_sub(at.timestamp());
                if !(0..=MAX_AGE_SECONDS).contains(&delta) {
                    errors.push(format!(
                        "result timestamp is future-dated or older than seven days: {}/{}",
                        key.0, key.1
                    ));
                }
            }
            None => errors.push(format!(
                "result timestamp missing or invalid: {}/{}",
                key.0, key.1
            )),
        }
        if row
            .get("error")
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
        {
            errors.push(format!(
                "passing result has failure reason: {}/{}",
                key.0, key.1
            ));
        }
        passed.insert(key);
    }
    for key in required_set
        .difference(&seen)
        .filter(|key| !waived.contains(*key))
    {
        errors.push(format!("missing result: {}/{}", key.0, key.1));
    }
    if passed.len() < MIN_PASSES {
        errors.push(format!(
            "release gate requires at least {MIN_PASSES} real passes; found {}",
            passed.len()
        ));
    }
    if passed.len() + waived.len() != 40 {
        errors.push(format!(
            "release gate requires 40 accounted actions; found {} passes and {} waivers",
            passed.len(),
            waived.len()
        ));
    }
    if failed > 0 {
        errors.push(format!(
            "{failed} failed action(s) block release; failures are never waived automatically"
        ));
    }
    if !errors.is_empty() {
        return Err(ReachError::new("BAD_INPUT", errors.join("; ")));
    }
    Ok(
        json!({"required": 40, "passed": passed.len(), "waived": waived.len(), "failed": failed, "release_ready": true, "source_sha256": source_digest}),
    )
}

fn pair(value: &Value) -> Option<(String, String)> {
    Some((
        value.get("channel")?.as_str()?.to_owned(),
        value.get("action")?.as_str()?.to_owned(),
    ))
}

fn allowed_waivers() -> BTreeMap<(String, String), String> {
    [
        (("reddit", "subreddit"), "Reddit's site tool returns HTML instead of data and subreddit has no public-web route yet; approved to ship untested in 1.6.1"),
        (("instagram", "search"), "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        (("instagram", "profile"), "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        (("instagram", "posts"), "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        (("bilibili", "subtitle"), "Upstream QR login saves an empty SESSDATA and the test Chrome has no Bilibili login; approved to ship untested in 1.6.1"),
        (("xiaoyuzhou", "transcribe"), "No Groq key on the test machine; approved to ship untested in 1.6.1"),
    ].into_iter().map(|((c, a), r)| ((c.into(), a.into()), r.into())).collect()
}
