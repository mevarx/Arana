//! Child-runner tests: bounded output, deadline with Windows process-tree
//! kill, and cancellation safety (a dropped future still kills the tree).

use reach_tools::runner::{home_dir, last_line, run_captured, run_cmd, CommandSpec};
use std::time::Duration;
use tokio::sync::Mutex;

/// Serialize tests that observe global process state (`ping.exe`).
static KILL_TEST_LOCK: Mutex<()> = Mutex::const_new(());

fn cmd() -> std::path::PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    std::path::PathBuf::from(root)
        .join("System32")
        .join("cmd.exe")
}

fn process_running(image: &str) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}")])
        .output()
        .expect("tasklist runs");
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    text.contains(&image.to_lowercase())
}

/// Poll until `image` is gone (bounded), so kill assertions don't race.
fn wait_until_gone(image: &str) {
    let start = std::time::Instant::now();
    while process_running(image) {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{image} still running after the child was killed"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn runs_a_command_and_captures_stdout() {
    let mut spec = CommandSpec::new(cmd(), "cmd");
    spec.args = vec!["/c".into(), "echo".into(), "hello-arana".into()];
    spec.timeout = Duration::from_secs(15);
    let out = run_captured(spec).await.expect("command runs");
    assert!(out.success());
    assert!(out.stdout.contains("hello-arana"), "{:?}", out.stdout);
    assert!(out.stderr.trim().is_empty(), "{:?}", out.stderr);
}

#[tokio::test]
async fn nonzero_exit_maps_to_upstream_broken_with_last_stderr_line() {
    let err = run_cmd(
        &[
            cmd().to_string_lossy().into_owned(),
            "/c".into(),
            "echo boom >&2 && exit 3".into(),
        ],
        Duration::from_secs(15),
        "cmd",
    )
    .await
    .expect_err("nonzero exit fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("exited 3"), "{}", err.message);
    assert!(err.message.contains("boom"), "{}", err.message);

    let ok = run_cmd(
        &[
            cmd().to_string_lossy().into_owned(),
            "/c".into(),
            "echo fine".into(),
        ],
        Duration::from_secs(15),
        "cmd",
    )
    .await
    .expect("zero exit succeeds");
    assert_eq!(ok.trim(), "fine");
}

#[tokio::test]
async fn missing_program_reports_need_setup() {
    let spec = CommandSpec::new("definitely-not-a-real-binary-xyz", "ghost");
    let err = run_captured(spec).await.expect_err("missing program fails");
    assert_eq!(err.code, "NEED_SETUP");
    assert!(err.message.contains("ghost"), "{}", err.message);
}

#[tokio::test]
async fn timeout_kills_the_whole_process_tree() {
    let _guard = KILL_TEST_LOCK.lock().await;
    assert!(!process_running("ping.exe"), "precondition: no stray ping");
    let mut spec = CommandSpec::new(cmd(), "cmd");
    spec.args = vec!["/c".into(), "ping -n 60 127.0.0.1".into()];
    spec.timeout = Duration::from_millis(1500);
    let start = std::time::Instant::now();
    let err = run_captured(spec).await.expect_err("slow child fails");
    assert_eq!(err.code, "TIMEOUT");
    assert!(err.message.contains("cmd"), "{}", err.message);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "must not wait for the child"
    );
    // The grandchild ping.exe must be dead: killing only the direct child
    // (cmd.exe) would orphan it.
    wait_until_gone("ping.exe");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_future_still_kills_the_tree() {
    let _guard = KILL_TEST_LOCK.lock().await;
    assert!(!process_running("ping.exe"), "precondition: no stray ping");
    let mut spec = CommandSpec::new(cmd(), "cmd");
    spec.args = vec!["/c".into(), "ping -n 60 127.0.0.1".into()];
    spec.timeout = Duration::from_secs(60);
    let task = tokio::spawn(run_captured(spec));
    tokio::time::sleep(Duration::from_millis(800)).await;
    task.abort();
    let _ = task.await;
    // Aborting the caller dropped the runner future; the tree must die anyway.
    wait_until_gone("ping.exe");
}

#[tokio::test]
async fn output_cap_kills_and_reports_overflow() {
    // A batch script that prints ~12 KB to stdout.
    let dir = tempfile::tempdir().unwrap();
    let bat = dir.path().join("noisy.bat");
    std::fs::write(
        &bat,
        "@echo off\r\nfor /l %%i in (1,1,300) do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
    )
    .unwrap();
    let mut spec = CommandSpec::new(&bat, "noisy");
    spec.timeout = Duration::from_secs(15);
    spec.max_output_bytes = 2048;
    let err = run_captured(spec)
        .await
        .expect_err("oversized output fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("exceeds"), "{}", err.message);
}

#[test]
fn home_dir_honors_explicit_home() {
    // HOME wins even on Windows (isolation boundary, port of paths.home_dir).
    let previous = std::env::var("HOME").ok();
    std::env::set_var("HOME", r"D:\definitely\home");
    assert_eq!(home_dir(), std::path::PathBuf::from(r"D:\definitely\home"));
    if let Some(prev) = previous {
        std::env::set_var("HOME", prev);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn last_line_returns_last_nonempty_line() {
    assert_eq!(last_line("a\nb\n\n  \n"), "b");
    assert_eq!(last_line(""), "");
    assert_eq!(last_line("\nonly"), "only");
}
