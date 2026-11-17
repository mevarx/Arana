use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn configure_stdin_never_echoes_secret_and_persists_privately() {
    let d = tempfile::tempdir().unwrap();
    let secret = "test-only-not-a-real-api-key";
    let mut child = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(["configure", "groq-key", "--stdin"])
        .env("ARANA_HOME", d.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for bytes in [&out.stdout, &out.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(secret));
    }
    let cfg: Value =
        serde_yaml::from_str(&std::fs::read_to_string(d.path().join("config.yaml")).unwrap())
            .unwrap();
    assert_eq!(cfg["groq_api_key"], secret);
    let output = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args(["configure", "groq-key"])
        .env("ARANA_HOME", d.path())
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["configured"], json!(true));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
}
