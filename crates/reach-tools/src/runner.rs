//! Robust bounded child runner.
//!
//! Ports `runtime/run.py` + `utils/process.py`: hidden Windows children
//! (`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`), a hard deadline, bounded
//! stdout/stderr capture, process-tree kill on timeout (`taskkill /T /F`) and
//! cancellation safety — dropping the future still kills the whole tree.

use reach_core::{ReachError, Result};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

/// One fully-resolved command to run.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    /// Absolute path of the executable (resolve first via registry).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Hard deadline for the whole run.
    pub timeout: Duration,
    /// Per-stream output cap in bytes.
    pub max_output_bytes: usize,
    /// Human label used in errors.
    pub label: String,
    pub cwd: Option<PathBuf>,
    /// Extra env entries layered over a UTF-8-forcing copy of the parent env.
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>, label: &str) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: Duration::from_secs(600),
            max_output_bytes: 8 * 1024 * 1024,
            label: label.to_string(),
            cwd: None,
            env: Vec::new(),
        }
    }
}

/// Captured result of a finished child.
#[derive(Debug)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status.success()
    }
    pub fn code(&self) -> i32 {
        self.status.code().unwrap_or(-1)
    }
}

/// The user's home directory: `HOME` wins even on Windows (isolation boundary,
/// port of upstream `paths.home_dir`).
pub fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    #[allow(deprecated)]
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Last non-empty line of a stream (upstream error-message convention).
pub fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
}

/// Kill an entire process tree, bounded. Synchronous (used from `Drop` and
/// rare error paths); `taskkill /T /F` normally completes in milliseconds.
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        let taskkill = std::env::var("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("taskkill.exe"))
            .unwrap_or_else(|_| PathBuf::from("taskkill.exe"));
        let Ok(mut child) = std::process::Command::new(&taskkill)
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
    #[cfg(not(windows))]
    {
        use std::process::Stdio;
        // The child was spawned as a process-group leader, so killing the
        // negative pid takes down the whole tree.
        let _ = std::process::Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// RAII tree-kill guard: armed while a child runs, disarmed once it is reaped.
struct TreeKillGuard {
    pid: Option<u32>,
}

impl TreeKillGuard {
    fn new(pid: u32) -> Self {
        Self { pid: Some(pid) }
    }
    fn kill_now(&mut self) {
        if let Some(pid) = self.pid.take() {
            kill_tree(pid);
        }
    }
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for TreeKillGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// Read one pipe up to `cap + 1` bytes. Returns `(bytes, overflow)`. On
/// overflow the reader is dropped, closing the pipe so a writer that keeps
/// printing gets EPIPE instead of blocking forever.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    cap: usize,
) -> (Vec<u8>, bool) {
    let mut buf: Vec<u8> = Vec::new();
    // Heap-backed scratch space keeps nested async process backends small
    // enough for the Windows main thread's default stack.
    let mut chunk = vec![0u8; 16384];
    use tokio::io::AsyncReadExt;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => return (buf, false),
            Ok(n) => {
                if buf.len() + n > cap {
                    let keep = cap.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..keep]);
                    return (buf, true);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => return (buf, false),
        }
    }
}

/// Run one command under the deadline, capturing bounded output. On timeout
/// or drop the whole process tree is killed. Returns the child's status and
/// both streams decoded as UTF-8 (lossy).
pub async fn run_captured(spec: CommandSpec) -> Result<CommandOutput> {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Safety net for the direct child even if this future is dropped.
        .kill_on_drop(true)
        // Force UTF-8 mode in Python children (port of utf8_subprocess_env).
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP (hidden children).
        command.creation_flags(0x0800_0000 | 0x0000_0200);
    }
    #[cfg(unix)]
    {
        // Make the child a process-group leader so the whole tree can be
        // killed with one signal.
        command.process_group(0);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    // Run from the home directory by default so tools never pick up config
    // from whatever project the agent happens to be in.
    let cwd = spec.cwd.clone().unwrap_or_else(home_dir);
    if cwd.exists() {
        command.current_dir(&cwd);
    }

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ReachError::new("NEED_SETUP", format!("{} is not installed", spec.label))
        } else {
            ReachError::from(e)
        }
    })?;
    let pid = child.id().unwrap_or(0);
    let mut guard = TreeKillGuard::new(pid);

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let cap = spec.max_output_bytes;
    let work = async {
        let (out, err) = tokio::join!(
            async {
                match stdout_pipe {
                    Some(pipe) => read_capped(pipe, cap).await,
                    None => (Vec::new(), false),
                }
            },
            async {
                match stderr_pipe {
                    Some(pipe) => read_capped(pipe, cap).await,
                    None => (Vec::new(), false),
                }
            }
        );
        let status = child.wait().await?;
        std::io::Result::Ok((out, err, status))
    };

    let joined = tokio::time::timeout(spec.timeout, work).await;
    match joined {
        Ok(Ok((out, err, status))) => {
            let (stdout, out_overflow) = out;
            let (stderr, err_overflow) = err;
            if out_overflow || err_overflow {
                guard.kill_now();
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!(
                        "{} output exceeds {} bytes",
                        spec.label, spec.max_output_bytes
                    ),
                ));
            }
            guard.disarm();
            Ok(CommandOutput {
                status,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
        Ok(Err(io_err)) => {
            guard.kill_now();
            Err(ReachError::from(io_err))
        }
        Err(_elapsed) => {
            // The timed future was dropped: its pipes and the Child handle
            // are gone (kill_on_drop took the direct child); finish the tree.
            guard.kill_now();
            Err(ReachError::new(
                "TIMEOUT",
                format!(
                    "{} did not finish within {:.0}s",
                    spec.label,
                    spec.timeout.as_secs_f64()
                ),
            ))
        }
    }
}

/// Convenience wrapper mirroring upstream `run_cmd`: resolves `argv[0]` on
/// PATH, runs from the home directory, returns stdout; a non-zero exit becomes
/// `UPSTREAM_BROKEN` carrying the last stderr line.
pub async fn run_cmd(argv: &[String], timeout: Duration, label: &str) -> Result<String> {
    if argv.is_empty() {
        return Err(ReachError::new("BAD_INPUT", "empty command"));
    }
    let program = PathBuf::from(&argv[0]);
    let resolved = if program.is_absolute() {
        program
    } else {
        which::which(&argv[0])
            .map_err(|_| ReachError::new("NEED_SETUP", format!("{label} is not installed")))?
    };
    let mut spec = CommandSpec::new(&resolved, label);
    spec.args = argv[1..].to_vec();
    spec.timeout = timeout;
    let out = run_captured(spec).await?;
    if !out.success() {
        let last = last_line(&out.stderr).trim();
        let message = if last.is_empty() {
            format!("{label} exited {}: no error output", out.code())
        } else {
            format!("{label} exited {}: {last}", out.code())
        };
        return Err(ReachError::new("UPSTREAM_BROKEN", message));
    }
    Ok(out.stdout)
}
