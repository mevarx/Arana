use serde_json::Value;
use std::process::Command;
fn invoke(args: &[&str]) -> (i32, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    // macOS temp dirs live under /var, itself a symlink to /private/var; hand
    // the binary a canonical root the way a real ARANA_HOME would be, so the
    // state guard is not fed a platform symlink.
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(args)
        .env("ARANA_HOME", &home)
        .output()
        .unwrap();
    let stdout = String::from_utf8(o.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must contain exactly one JSON object: {stdout}"
    );
    (
        o.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}
#[test]
fn browser_route_prefers_host_and_only_considers_explicit_legacy_fallback() {
    for (args, route) in [
        (
            vec!["available", "--allow-chrome", "twitter", "read"],
            "host_browser",
        ),
        (vec!["unavailable"], "unavailable"),
        (
            vec!["unknown", "--allow-chrome", "twitter", "thread"],
            "unknown",
        ),
        (
            vec!["denied", "--allow-chrome", "twitter", "thread"],
            "blocked",
        ),
        (
            vec!["unavailable", "--allow-chrome", "web", "read"],
            "unsupported",
        ),
    ] {
        let mut command = vec!["browser", "route"];
        command.extend(args);
        let (code, body, _) = invoke(&command);
        assert_eq!(code, 0, "{body}");
        assert_eq!(body["result"]["route"], route, "{body}");
    }
    let (code, body, _) = invoke(&["browser", "route", "auto"]);
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
}

#[test]
fn browser_route_detects_chrome_and_opencli_without_launching_them() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("arana-state");
    let executable = env!("CARGO_BIN_EXE_arana");
    let output = Command::new(executable)
        .args([
            "browser",
            "route",
            "unavailable",
            "--allow-chrome",
            "twitter",
            "thread",
        ])
        .env("ARANA_HOME", &home)
        .env("ARANA_CHROME", executable)
        .env("ARANA_OPENCLI_BIN", executable)
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["result"]["route"], "chrome_opencli");
    assert_eq!(body["result"]["bridge_verified"], false);
    assert!(
        !home.exists(),
        "routing must not create browser or state files"
    );
}

#[test]
fn catalog_and_alias_are_identical() {
    let (code, v, _) = invoke(&["channels"]);
    assert_eq!(code, 0);
    assert_eq!(v["channels"].as_array().unwrap().len(), 14);
    let n: usize = v["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["actions"].as_array().unwrap().len())
        .sum();
    assert_eq!(n, 40);
    let out = Command::new(env!("CARGO_BIN_EXE_agent-reach"))
        .arg("channels")
        .output()
        .unwrap();
    let alias: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, alias);
}
#[test]
fn invalid_inputs_are_json_and_fail_without_side_effects() {
    for args in [
        vec!["unknown", "read"],
        vec!["web", "unknown"],
        vec!["web", "read"],
        vec!["web", "search", "x", "--limit", "0"],
        vec!["web", "search", "x", "--timeout", "nan"],
        vec!["web", "search", "x", "--wat"],
        vec!["web", "read", "https://example.com", "--backend", "missing"],
    ] {
        let (code, v, _) = invoke(&args);
        assert_ne!(code, 0);
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "BAD_INPUT");
    }
}
#[test]
fn version_and_browser_status_work_without_network() {
    let (code, v, _) = invoke(&["version"]);
    assert_eq!(code, 0, "version failed: {v}");
    assert!(v["result"]["version"].is_string());
    let (code, v, stderr) = invoke(&["browser", "status"]);
    assert_eq!(code, 0, "browser status failed: {v} {stderr}");
    assert!(v["result"]["running"].is_boolean());
}
