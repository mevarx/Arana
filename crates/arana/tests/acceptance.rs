use serde_json::Value;
use std::process::Command;

fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "CLI must emit one JSON line: {stdout}"
    );
    (
        output.status.code().unwrap_or(-1),
        serde_json::from_str(&stdout).unwrap(),
    )
}

#[test]
fn shipped_executable_reports_its_embedded_source_signature_and_40_action_catalog() {
    let (code, version) = run(&["version"]);
    assert_eq!(code, 0);
    let signature = version["result"]["source_sha256"]
        .as_str()
        .expect("binary source signature");
    assert_eq!(signature.len(), 64);
    assert!(signature.bytes().all(|b| b.is_ascii_hexdigit()));
    let (code, channels) = run(&["channels"]);
    assert_eq!(code, 0);
    let actions: usize = channels["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["actions"].as_array().unwrap().len())
        .sum();
    assert_eq!(actions, 40);
}

fn validate_recorded_manifest(path: &std::path::Path) -> reach_core::Result<Value> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(path)?).map_err(|_| {
        reach_core::ReachError::new("BAD_INPUT", "Acceptance manifest is invalid JSON")
    })?;
    let (version_code, version) = run(&["version"]);
    let (catalog_code, catalog) = run(&["channels"]);
    if version_code != 0 || catalog_code != 0 {
        return Err(reach_core::ReachError::new(
            "BAD_INPUT",
            "Acceptance binary identity is unavailable",
        ));
    }
    let signature = version["result"]["source_sha256"].as_str().ok_or_else(|| {
        reach_core::ReachError::new("BAD_INPUT", "Missing binary source signature")
    })?;
    let mut required = Vec::new();
    for channel in catalog["channels"]
        .as_array()
        .expect("real catalog channels")
    {
        for action in channel["actions"].as_array().expect("real catalog actions") {
            required.push((
                channel["name"].as_str().expect("channel name").to_owned(),
                action["name"].as_str().expect("action name").to_owned(),
            ));
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    reach_core::acceptance::validate(&manifest, &required, signature, now)
}

#[test]
fn recorded_manifest_is_checked_against_the_actual_binary_not_only_self_claimed_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = temp.path().join("manifest.json");
    std::fs::write(&manifest, "{}").unwrap();
    assert_eq!(
        validate_recorded_manifest(&manifest).unwrap_err().code,
        "BAD_INPUT"
    );
}

/// Run explicitly with `cargo test -p arana --test acceptance -- --ignored --nocapture`.
/// Requires real platforms/accounts; every outcome is recorded as a real probe, never skipped.
#[test]
#[ignore = "live acceptance intentionally requires operator-approved accounts and network"]
fn live_40_action_release_acceptance_probe() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = root.join("scripts/record_release_acceptance.py");
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_arana"));
    let status = Command::new("python")
        .arg(script)
        .arg("--binary")
        .arg(binary)
        .arg("--out")
        .arg(root.join(".github/release-acceptance.json"))
        .status()
        .expect("run real acceptance harness");
    let result = validate_recorded_manifest(&root.join(".github/release-acceptance.json"));
    assert!(status.success()&&result.is_ok(),"release acceptance failed or remains pending; inspect the recorder output and Rust gate: {result:?}");
}
