use chrono::{Duration, Utc};
use reach_core::acceptance::validate;
use serde_json::{json, Value};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn required() -> Vec<(String, String)> {
    [
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
    ]
    .into_iter()
    .map(|(c, a)| (c.into(), a.into()))
    .collect()
}
fn passing(channel: &str, action: &str) -> Value {
    json!({"channel":channel,"action":action,"method":"real-cli","backend":"fixture-backend","item_count":1,"at":Utc::now().to_rfc3339(),"os":"Windows","source_sha256":DIGEST,"passed":true,"error":null})
}
fn manifest() -> Value {
    let waived = [
        ("reddit", "subreddit", "Reddit's site tool returns HTML instead of data and subreddit has no public-web route yet; approved to ship untested in 1.6.1"),
        ("instagram", "search", "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        ("instagram", "profile", "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        ("instagram", "posts", "No Instagram login on the test machine; approved to ship untested in 1.6.1"),
        ("bilibili", "subtitle", "Upstream QR login saves an empty SESSDATA and the test Chrome has no Bilibili login; approved to ship untested in 1.6.1"),
        ("xiaoyuzhou", "transcribe", "No Groq key on the test machine; approved to ship untested in 1.6.1"),
    ].into_iter().map(|(channel, action, reason)| json!({"channel":channel,"action":action,"reason":reason})).collect::<Vec<_>>();
    let waived_keys = [
        "reddit/subreddit",
        "instagram/search",
        "instagram/profile",
        "instagram/posts",
        "bilibili/subtitle",
        "xiaoyuzhou/transcribe",
    ];
    let results = required()
        .into_iter()
        .filter(|(c, a)| !waived_keys.contains(&format!("{c}/{a}").as_str()))
        .map(|(c, a)| passing(&c, &a))
        .collect::<Vec<_>>();
    json!({"source_sha256":DIGEST,"release_ready":true,"pending":[],"waived":waived,"results":results})
}
#[test]
fn accepts_34_real_results_and_six_exact_waivers() {
    let result = validate(&manifest(), &required(), DIGEST, Utc::now().timestamp()).unwrap();
    assert_eq!(result["required"], 40);
    assert_eq!(result["passed"], 34);
    assert_eq!(result["waived"], 6);
}
#[test]
fn rejects_duplicate_result_rows() {
    let mut m = manifest();
    m["results"]
        .as_array_mut()
        .unwrap()
        .push(passing("web", "search"));
    assert!(validate(&m, &required(), DIGEST, Utc::now().timestamp()).is_err());
}
#[test]
fn rejects_stale_result_even_if_marked_passed() {
    let mut m = manifest();
    m["results"][0]["at"] = json!((Utc::now() - Duration::days(8)).to_rfc3339());
    assert!(validate(&m, &required(), DIGEST, Utc::now().timestamp()).is_err());
}
#[test]
fn failed_rows_are_not_counted_as_pass_or_waiver() {
    let mut m = manifest();
    m["results"][0]["passed"] = json!(false);
    m["results"][0]["error"] = json!("NEED_LOGIN");
    assert!(validate(&m, &required(), DIGEST, Utc::now().timestamp()).is_err());
}
#[test]
fn malformed_pass_metadata_is_not_accepted_as_success() {
    for (field, value) in [
        ("backend", json!("  ")),
        ("os", json!("\t")),
        ("error", json!([])),
        ("error", json!({"code":"UPSTREAM_BROKEN"})),
    ] {
        let mut data = manifest();
        data["results"][0][field] = value;
        assert!(
            validate(&data, &required(), DIGEST, Utc::now().timestamp()).is_err(),
            "malformed pass accepted: {field}"
        );
    }
}
#[test]
fn requires_exactly_40_actions_and_matching_source_digest() {
    assert!(validate(
        &manifest(),
        &required()[..39],
        DIGEST,
        Utc::now().timestamp()
    )
    .is_err());
    assert!(validate(
        &manifest(),
        &required(),
        &"b".repeat(64),
        Utc::now().timestamp()
    )
    .is_err());
}
