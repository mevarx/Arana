//! Child-runner tests: bounded output, a deadline that kills the whole process
//! tree, and cancellation safety (a dropped future still kills the tree).
//!
//! Fixtures are built from the platform shell (`cmd.exe /c` on Windows,
//! `/bin/sh -c` elsewhere) and process-tree assertions from the platform
//! process lister (`tasklist` on Windows, `ps` on unix), so every test proves
//! the same thing on both OS families.

use reach_tools::runner::{home_dir, last_line, run_captured, run_cmd, CommandSpec};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Serialize tests that observe global process state (a stray `ping.exe` or
/// `sleep` would break their preconditions).
static KILL_TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// The platform shell as an absolute path, plus the flag that hands it a
/// one-shot script.
#[cfg(windows)]
fn shell() -> (PathBuf, &'static str) {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    (PathBuf::from(root).join("System32").join("cmd.exe"), "/c")
}

/// The platform shell as an absolute path, plus the flag that hands it a
/// one-shot script. macOS ships bash as `/bin/sh`; Linux uses dash or bash.
#[cfg(unix)]
fn shell() -> (PathBuf, &'static str) {
    (PathBuf::from("/bin/sh"), "-c")
}

/// `[shell, flag, script]`: the argv shape [`run_cmd`] takes.
fn shell_argv(script: &str) -> Vec<String> {
    let (program, flag) = shell();
    vec![
        program.to_string_lossy().into_owned(),
        flag.to_string(),
        script.to_string(),
    ]
}

/// A [`CommandSpec`] running `script` through the platform shell.
fn shell_spec(script: &str, label: &str) -> CommandSpec {
    let (program, flag) = shell();
    let mut spec = CommandSpec::new(program, label);
    spec.args = vec![flag.to_string(), script.to_string()];
    spec
}

/// A snapshot of the running processes, one per line. Windows: `tasklist` in
/// CSV mode without the header row, so every line is a real process and image
/// names are not truncated to 25 characters the way table mode truncates them.
#[cfg(windows)]
fn process_list() -> String {
    let out = std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A snapshot of the running processes as `pid command-line` lines. `ps -A -o
/// pid=,command=` is the form Linux and macOS both accept (macOS has no
/// `pgrep` by default); `-A` also covers processes without a controlling
/// terminal, which is what a piped child has, and `-w -w` lifts the 80-column
/// cut macOS otherwise applies to `command`.
#[cfg(unix)]
fn process_list() -> String {
    let out = std::process::Command::new("ps")
        .args(["-A", "-w", "-w", "-o", "pid=,command="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Which of `needles` still appear in a fresh process listing
/// (case-insensitive substring, so an image name or a distinctive argv token).
fn still_alive(needles: &[&str]) -> Vec<String> {
    let listing = process_list().to_lowercase();
    // An empty listing would make every "is it gone" check pass for free.
    assert!(
        !listing.trim().is_empty(),
        "process listing came back empty (ps/tasklist failed?)"
    );
    needles
        .iter()
        .copied()
        .filter(|needle| listing.contains(&needle.to_lowercase()))
        .map(str::to_string)
        .collect()
}

/// Precondition: no process from an earlier fixture run is still around, so a
/// later match can only be *our* child.
fn assert_no_stray(needles: &[&str]) {
    let alive = still_alive(needles);
    assert!(alive.is_empty(), "precondition: no stray {alive:?}");
}

/// Poll until nothing from the fixture is left (bounded), so the kill
/// assertions do not race the kernel's teardown.
async fn wait_until_gone(needles: &[&str]) {
    let start = Instant::now();
    loop {
        let alive = still_alive(needles);
        if alive.is_empty() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{alive:?} still running after the child was killed"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Poll until at least one process from the fixture is visible, so a kill test
/// cannot pass just because the tree never got off the ground.
async fn wait_until_alive(needles: &[&str]) {
    let start = Instant::now();
    while still_alive(needles).is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "fixture {needles:?} never started"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The process tree the kill tests spawn, plus the argv tokens that only that
/// tree can produce (used to prove it is dead).
struct TreeFixture {
    spec: CommandSpec,
    needles: &'static [&'static str],
}

/// Windows: `cmd.exe` -> `ping.exe`. `ping` is the child a non-recursive kill
/// would orphan, and `tasklist` reports it by image name.
#[cfg(windows)]
fn tree_fixture() -> TreeFixture {
    TreeFixture {
        spec: shell_spec("ping -n 60 127.0.0.1", "cmd"),
        needles: &["ping.exe"],
    }
}

/// Unix: `sh` -> `sh` -> `sleep`, three levels deep. The `&` + `wait` keeps the
/// outer shell alive even when a shell optimises `sh -c 'cmd'` into an `exec`,
/// and 31415s is a duration nothing else on a CI box asks for, so a `ps` match
/// means our fixture survived.
#[cfg(unix)]
fn tree_fixture() -> TreeFixture {
    TreeFixture {
        spec: shell_spec("sh -c 'sleep 31415' & wait", "sh"),
        needles: &["sleep 31415"],
    }
}

#[tokio::test]
async fn runs_a_command_and_captures_stdout() {
    let mut spec = shell_spec("echo hello-arana", "shell");
    spec.timeout = Duration::from_secs(15);
    let out = run_captured(spec).await.expect("command runs");
    assert!(out.success());
    assert!(out.stdout.contains("hello-arana"), "{:?}", out.stdout);
    assert!(out.stderr.trim().is_empty(), "{:?}", out.stderr);
}

#[tokio::test]
async fn nonzero_exit_maps_to_upstream_broken_with_last_stderr_line() {
    let err = run_cmd(
        &shell_argv("echo boom >&2 && exit 3"),
        Duration::from_secs(15),
        "shell",
    )
    .await
    .expect_err("nonzero exit fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("exited 3"), "{}", err.message);
    assert!(err.message.contains("boom"), "{}", err.message);

    let ok = run_cmd(&shell_argv("echo fine"), Duration::from_secs(15), "shell")
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
    let mut fixture = tree_fixture();
    assert_no_stray(fixture.needles);
    fixture.spec.timeout = Duration::from_millis(1500);
    let expected = format!("{} did not finish", fixture.spec.label);
    let start = Instant::now();
    let err = run_captured(fixture.spec)
        .await
        .expect_err("slow child fails");
    assert_eq!(err.code, "TIMEOUT");
    assert!(err.message.contains(&expected), "{}", err.message);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "must not wait for the child"
    );
    // The descendant must be dead. Killing only the direct child would orphan
    // it: on Windows the runner uses a recursive `taskkill`, on unix it kills
    // the child's whole process group, which is where the descendant lives.
    wait_until_gone(fixture.needles).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_future_still_kills_the_tree() {
    let _guard = KILL_TEST_LOCK.lock().await;
    let mut fixture = tree_fixture();
    assert_no_stray(fixture.needles);
    fixture.spec.timeout = Duration::from_secs(60);
    let needles = fixture.needles;
    let task = tokio::spawn(run_captured(fixture.spec));
    // Only abort once the tree is up, and give it a moment to reach full depth
    // (outer shell -> nested shell -> sleeper), so passing cannot mean "nothing
    // ran" or "only the outermost process had started".
    wait_until_alive(needles).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();
    let _ = task.await;
    // Aborting the caller dropped the runner future; the tree must die anyway.
    wait_until_gone(needles).await;
}

/// A script that prints ~16 KB to stdout, well past the 2 KB cap below.
#[cfg(windows)]
fn noisy_spec(dir: &Path) -> CommandSpec {
    let bat = dir.join("noisy.bat");
    std::fs::write(
        &bat,
        "@echo off\r\nfor /l %%i in (1,1,300) do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
    )
    .unwrap();
    CommandSpec::new(&bat, "noisy")
}

/// The same flood from a POSIX shell loop. The script is handed to the shell as
/// an argument, so it needs neither a shebang nor the executable bit.
#[cfg(unix)]
fn noisy_spec(dir: &Path) -> CommandSpec {
    let script = dir.join("noisy.sh");
    std::fs::write(
        &script,
        "i=0\nwhile [ \"$i\" -lt 300 ]; do\n  echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n  i=$((i + 1))\ndone\n",
    )
    .unwrap();
    let (program, _) = shell();
    let mut spec = CommandSpec::new(program, "noisy");
    spec.args = vec![script.to_string_lossy().into_owned()];
    spec
}

#[tokio::test]
async fn output_cap_kills_and_reports_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = noisy_spec(dir.path());
    spec.timeout = Duration::from_secs(15);
    spec.max_output_bytes = 2048;
    let err = run_captured(spec)
        .await
        .expect_err("oversized output fails");
    assert_eq!(err.code, "UPSTREAM_BROKEN");
    assert!(err.message.contains("exceeds"), "{}", err.message);
}

/// A home directory that cannot exist, spelled natively for the platform.
#[cfg(windows)]
const FAKE_HOME: &str = r"D:\definitely\home";
#[cfg(unix)]
const FAKE_HOME: &str = "/definitely/home";

#[test]
fn home_dir_honors_explicit_home() {
    // HOME wins even on Windows (isolation boundary, port of paths.home_dir).
    let previous = std::env::var_os("HOME");
    std::env::set_var("HOME", FAKE_HOME);
    let resolved = home_dir();
    match previous {
        Some(prev) => std::env::set_var("HOME", prev),
        None => std::env::remove_var("HOME"),
    }
    assert_eq!(resolved, PathBuf::from(FAKE_HOME));
}

#[test]
fn last_line_returns_last_nonempty_line() {
    assert_eq!(last_line("a\nb\n\n  \n"), "b");
    assert_eq!(last_line(""), "");
    assert_eq!(last_line("\nonly"), "only");
}
