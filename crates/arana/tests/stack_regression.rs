use std::process::Command;

/// A stand-in backend that always fails with a message on stderr. The
/// override must point at a real executable file: `ARANA_OBSCURA_BIN` is only
/// honoured when it is one, and `/bin/false` does not exist on macOS, which
/// silently degraded the run to NEED_SETUP instead of exercising the backend.
#[cfg(unix)]
fn failing_backend(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("failing-obscura.sh");
    std::fs::write(&path, "#!/bin/sh\necho 'engine exploded' >&2\nexit 7\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn obscura_backend_failure_still_returns_one_json_instead_of_overflowing_stack() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let executable = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/whoami.exe");
    #[cfg(unix)]
    let executable = failing_backend(dir.path());
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
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "the backend failure must carry a message: {json}"
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().lines().count(), 1);
}
