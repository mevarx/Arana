use std::process::Command;
#[test]
fn obscura_backend_failure_still_returns_one_json_instead_of_overflowing_stack() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let executable = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/whoami.exe");
    #[cfg(not(windows))]
    let executable = std::path::PathBuf::from("/bin/false");
    let out = Command::new(env!("CARGO_BIN_EXE_arana"))
        .args([
            "web",
            "read",
            "https://example.com",
            "--backend",
            "obscura",
            "--timeout",
            "3",
        ])
        .env("ARANA_HOME", dir.path())
        .env("ARANA_OBSCURA_BIN", executable)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "backend must not overflow/panic: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "UPSTREAM_BROKEN");
    assert_eq!(String::from_utf8(out.stdout).unwrap().lines().count(), 1);
}
