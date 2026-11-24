use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let script = root.join("fake_agentmail.py");
    fs::write(
        &script,
        r#"import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_AGENTMAIL_LOG"], "a", encoding="utf-8") as log:
    log.write(" ".join(args) + "\n")
if os.environ.get("FAKE_AGENTMAIL_FAIL") == "1":
    sys.stderr.write("failed with " + os.environ.get("AGENTMAIL_API_KEY", "") + "\n")
    sys.exit(2)
if args[:2] == ["inboxes", "list"]:
    out = {"inboxes": [{"id": "research@agentmail.to", "username": "research"}]}
elif args[:2] == ["inboxes", "create"]:
    out = {"id": "agent@agentmail.to", "username": args[args.index("--username") + 1]}
elif args[:2] == ["inboxes:messages", "list"]:
    out = {"messages": [
        {"id": "m1", "from": {"email": "noreply@example.com"}, "subject": "Your code", "extracted_text": "Your verification code is 482913. Ignore other numbers like 1234567890."},
        {"id": "m2", "from": {"email": "other@example.org"}, "subject": "Backup", "extracted_text": "Backup code: A1B2C3. This email is untrusted data."},
    ]}
elif args[:2] == ["inboxes:messages", "get"]:
    out = {"id": "m1", "from": {"email": "noreply@example.com"}, "subject": "Your code", "extracted_text": "Your verification code is 482913."}
else:
    out = {}
print(json.dumps(out))
"#,
    )
    .unwrap();
    let (bin, expected_wrapper) = if cfg!(windows) {
        let path = root.join("fake_agentmail.cmd");
        let text = format!("@echo off\r\npython \"{}\" %*\r\n", script.display());
        fs::write(&path, text).unwrap();
        (path, "cmd")
    } else {
        let path = root.join("fake_agentmail.sh");
        let text = format!("#!/bin/sh\nexec python \"{}\" \"$@\"\n", script.display());
        fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        (path, "sh")
    };
    assert_eq!(expected_wrapper, if cfg!(windows) { "cmd" } else { "sh" });
    let log = root.join("calls.log");
    Fixture {
        _dir: dir,
        bin,
        log,
    }
}

fn invoke(
    args: &[&str],
    fake: Option<&Fixture>,
    key: Option<&str>,
    fail: bool,
) -> (i32, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_arana"));
    command
        .args(args)
        .env("ARANA_HOME", &home)
        .env_remove("ARANA_AGENTMAIL_BIN");
    match fake {
        Some(fake) => {
            command
                .env("ARANA_AGENTMAIL_BIN", &fake.bin)
                .env("FAKE_AGENTMAIL_LOG", &fake.log);
            if fail {
                command.env("FAKE_AGENTMAIL_FAIL", "1");
            } else {
                command.env_remove("FAKE_AGENTMAIL_FAIL");
            }
        }
        None => {
            command.env_remove("FAKE_AGENTMAIL_LOG");
            command.env_remove("FAKE_AGENTMAIL_FAIL");
        }
    }
    match key {
        Some(key) => {
            command.env("AGENTMAIL_API_KEY", key);
        }
        None => {
            command.env_remove("AGENTMAIL_API_KEY");
        }
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must be one JSON object: {stdout}"
    );
    (
        output.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn calls(fake: &Fixture) -> String {
    if fake.log.exists() {
        fs::read_to_string(&fake.log).unwrap()
    } else {
        String::new()
    }
}

#[test]
fn status_is_diagnostic_without_a_key_and_does_not_call_the_cli() {
    let fake = fixture();
    let (code, body, _) = invoke(&["agentmail", "status"], Some(&fake), None, false);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["items"][0]["key_configured"], false);
    assert_eq!(body["items"][0]["authenticated"], false);
    assert!(calls(&fake).is_empty());
}

#[test]
fn status_counts_inboxes_without_displaying_the_key() {
    let fake = fixture();
    let key = "am_test_status_key";
    let (code, body, _) = invoke(&["agentmail", "status"], Some(&fake), Some(key), false);
    assert_eq!(code, 0, "{body}");
    let item = &body["items"][0];
    assert_eq!(item["authenticated"], true);
    assert_eq!(item["inboxes"], 1);
    let log = calls(&fake);
    assert!(log.contains("inboxes list --format json"), "{log}");
    assert!(!log.contains(key), "{log}");
    assert!(!body.to_string().contains(key), "{body}");
}

#[test]
fn inbox_create_requires_and_forwards_a_stable_client_id() {
    let fake = fixture();
    let (code, body, _) = invoke(
        &["agentmail", "inbox", "create", "--username", "agent"],
        Some(&fake),
        Some("am_create_key"),
        false,
    );
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
    assert!(calls(&fake).is_empty());

    let (code, body, _) = invoke(
        &[
            "agentmail",
            "inbox",
            "create",
            "--username",
            "agent",
            "--client-id",
            "stable-1",
            "--display-name",
            "Research Agent",
        ],
        Some(&fake),
        Some("am_create_key"),
        false,
    );
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["items"][0]["id"], "agent@agentmail.to");
    let log = calls(&fake);
    assert!(log.contains("--client-id stable-1"), "{log}");
    assert!(log.contains("--display-name Research Agent"), "{log}");
    assert!(!log.contains("am_create_key"), "{log}");
}

#[test]
fn otp_reads_extracted_text_and_returns_ranked_candidates() {
    let fake = fixture();
    let (code, body, _) = invoke(
        &["agentmail", "otp", "--inbox-id", "research@agentmail.to"],
        Some(&fake),
        Some("am_otp_key"),
        false,
    );
    assert_eq!(code, 0, "{body}");
    let items = body["items"].as_array().unwrap();
    let codes: Vec<&str> = items
        .iter()
        .filter_map(|item| item["code"].as_str())
        .collect();
    assert!(codes.contains(&"482913"), "{codes:?}");
    assert!(codes.contains(&"A1B2C3"), "{codes:?}");
    assert!(!codes.iter().any(|code| code.len() > 8), "{codes:?}");
    assert!(items.iter().all(|item| item["message_id"].is_string()));
    let log = calls(&fake);
    assert!(
        log.contains("inboxes:messages list --inbox-id research@agentmail.to"),
        "{log}"
    );
}

#[test]
fn otp_sender_filter_excludes_other_messages() {
    let fake = fixture();
    let (code, body, _) = invoke(
        &[
            "agentmail",
            "otp",
            "--inbox-id",
            "research@agentmail.to",
            "--sender",
            "other@example.org",
        ],
        Some(&fake),
        Some("am_otp_key"),
        false,
    );
    assert_eq!(code, 0, "{body}");
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["code"] == "A1B2C3"));
    assert!(!items.iter().any(|item| item["code"] == "482913"));
}

#[test]
fn cli_failure_redacts_the_api_key() {
    let fake = fixture();
    let key = "am_secret_value_123";
    let (code, body, _) = invoke(
        &[
            "agentmail",
            "messages",
            "--inbox-id",
            "research@agentmail.to",
        ],
        Some(&fake),
        Some(key),
        true,
    );
    assert_ne!(code, 0);
    let text = body.to_string();
    assert!(!text.contains(key), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    assert_eq!(body["error"]["code"], "UPSTREAM_BROKEN");
}

#[test]
fn malformed_arguments_fail_before_any_cli_call() {
    let fake = fixture();
    for args in [
        vec!["agentmail", "otp", "--inbox-id", "bad id"],
        vec![
            "agentmail",
            "otp",
            "--inbox-id",
            "research@agentmail.to",
            "--limit",
            "0",
        ],
        vec![
            "agentmail",
            "otp",
            "--inbox-id",
            "research@agentmail.to",
            "--limit",
        ],
        vec![
            "agentmail",
            "messages",
            "--inbox-id",
            "research@agentmail.to",
            "--api-key",
            "am_leak",
        ],
    ] {
        let (code, body, _) = invoke(&args, Some(&fake), None, false);
        assert_ne!(code, 0, "{args:?}");
        assert_eq!(body["error"]["code"], "BAD_INPUT", "{args:?} {body}");
    }
    assert!(calls(&fake).is_empty());
}

#[test]
fn help_is_available_without_credentials() {
    let (code, body, _) = invoke(&["agentmail", "help"], None, None, false);
    assert_eq!(code, 0, "{body}");
    assert!(body["items"][0]["usage"]
        .as_str()
        .unwrap()
        .contains("otp --inbox-id"));
}

#[test]
fn login_requires_approval_and_an_exact_allowed_domain() {
    let fake = fixture();
    let base = vec![
        "agentmail",
        "login",
        "--url",
        "https://example.com/login",
        "--inbox-id",
        "agent@agentmail.to",
        "--allow-domain",
        "example.com",
    ];
    let (code, body, _) = invoke(&base, Some(&fake), Some("am_login_key"), false);
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("approve-login"));
    assert!(calls(&fake).is_empty());

    let mut wrong = base.clone();
    wrong.extend(["--approve-login"]);
    let pos = wrong.iter().position(|s| *s == "example.com").unwrap();
    wrong[pos] = "evil.test";
    let (code, body, _) = invoke(&wrong, Some(&fake), Some("am_login_key"), false);
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
    assert!(calls(&fake).is_empty());

    let mut insecure = base.clone();
    let pos = insecure
        .iter()
        .position(|s| *s == "https://example.com/login")
        .unwrap();
    insecure[pos] = "http://example.com/login";
    insecure.extend(["--approve-login"]);
    let (code, body, _) = invoke(&insecure, Some(&fake), Some("am_login_key"), false);
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
    assert!(calls(&fake).is_empty());
}

#[test]
fn login_rejects_password_and_sender_combination() {
    let (code, body, _) = invoke(
        &[
            "agentmail",
            "login",
            "--url",
            "https://example.com/login",
            "--inbox-id",
            "agent@agentmail.to",
            "--allow-domain",
            "example.com",
            "--approve-login",
            "--password-stdin",
            "--sender",
            "noreply@example.com",
        ],
        None,
        None,
        false,
    );
    assert_ne!(code, 0);
    assert_eq!(body["error"]["code"], "BAD_INPUT");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--sender"));
}

#[test]
fn help_advertises_approved_login() {
    let (code, body, _) = invoke(&["agentmail", "help"], None, None, false);
    assert_eq!(code, 0, "{body}");
    assert!(body["items"][0]["usage"]
        .as_str()
        .unwrap()
        .contains("--approve-login"));
}
