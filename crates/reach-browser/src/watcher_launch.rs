//! POSIX launch ownership: the hidden watcher owns the CDP socket/pipe and
//! Chromium child. Foreground callers wait for a per-launch readiness token.
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

fn child_pipe(command: &mut Command) -> Result<pipe::PipeConnection> {
    let (parent, child) = UnixStream::pair()?;
    // Duplicate beyond fd4 before mapping so neither dup2 clobbers its source.
    let raw = unsafe { libc::fcntl(child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let inherited = unsafe { OwnedFd::from_raw_fd(raw) };
    drop(child);
    // SAFETY: this post-fork callback only performs async-signal-safe fd ops.
    // inherited remains alive in the parent until Command::spawn returns.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(raw, 3) < 0 || libc::dup2(raw, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // raw retains CLOEXEC; only fd3/fd4 survive exec. Do not close
            // it here while the captured OwnedFd still owns the descriptor.
            Ok(())
        });
    }
    // The cloned child descriptor must survive until spawn; retain it in a
    // command-owned pre_exec closure, which is dropped in the parent afterward.
    unsafe {
        command.pre_exec(move || {
            let _keep_alive = &inherited;
            Ok(())
        });
    }
    parent.set_nonblocking(true)?;
    Ok(pipe::PipeConnection::from_io(
        tokio::net::UnixStream::from_std(parent)?,
    ))
}

pub(super) async fn start(state: &Path, headed: bool, url: &str) -> Result<Child> {
    let token = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let binary = std::env::var_os("ARANA_WATCHER_BIN")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let mut child = Command::new(binary)
        .arg("__browser-watch")
        .arg(state)
        .arg("--launch")
        .arg(if headed { "headed" } else { "headless" })
        .arg(url)
        .arg(&token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let ready = browser_root(state).join("watcher-ready.json");
    loop {
        if let Some(_status) = child.try_wait()? {
            return Err(ReachError::new(
                "UPSTREAM_BROKEN",
                "Browser watcher exited before Chromium was ready",
            ));
        }
        let matches = reach_core::state::read_private(&ready, 4096)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .is_some_and(|v| v["launch_id"] == token);
        if matches && running(state).await && owned(state).await? {
            return Ok(child);
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            return Err(ReachError::new(
                "TIMEOUT",
                "Browser watcher did not become ready within 90 seconds",
            ));
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn run(state: &Path, headed: bool, url: &str, token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > 100
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "Invalid watcher readiness token",
        ));
    }
    if url != "about:blank" {
        let parsed = url::Url::parse(url)
            .map_err(|_| ReachError::new("BAD_INPUT", "Invalid browser launch URL"))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(ReachError::new(
                "BAD_INPUT",
                "Browser launch requires a credential-free HTTP(S) URL",
            ));
        }
    }
    let root = browser_root(state);
    private_dir(&root)?;
    let lock_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let _lock = loop {
        match watcher_lock(&root) {
            Ok(v) => break v,
            Err(e) if e.code == "BUSY" && tokio::time::Instant::now() < lock_deadline => {
                sleep(Duration::from_millis(50)).await
            }
            Err(e) => return Err(e),
        }
    };
    if running(state).await {
        return Err(ReachError::new(
            "BUSY",
            "A browser already occupies the configured CDP port",
        ));
    }
    let profile = profile_path(state);
    private_dir(&profile)?;
    touch(state)?;
    let executable = chrome()
        .ok_or_else(|| ReachError::new("NEED_SETUP", "Chrome or Chromium was not found"))?;
    let mut command = Command::new(executable);
    command
        .arg(format!("--remote-debugging-port={}", port(state)))
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--remote-debugging-pipe",
            "--enable-automation",
            "--enable-unsafe-extension-debugging",
            "--no-first-run",
            "--no-default-browser-check",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if !headed {
        command.arg("--headless=new");
    }
    command.arg(url);
    let connection = child_pipe(&mut command)?;
    let mut child = command.spawn()?;
    // Drop pre_exec closures in the parent, closing its extra child endpoint.
    drop(command);
    let outcome = async {
        connection.call("Browser.getVersion", json!({})).await?;
        let extension = browser_root(state).join("extension");
        if extension.join("manifest.json").is_file() {
            let loaded = connection
                .call("Extensions.loadUnpacked", json!({"path":extension}))
                .await?;
            if loaded["id"].as_str().is_none() {
                return Err(ReachError::new(
                    "NEED_SETUP",
                    "OpenCLI extension failed to load in the dedicated browser",
                ));
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while !running(state).await {
            if child.try_wait()?.is_some() {
                return Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    "Chromium exited during pipe startup",
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ReachError::new("TIMEOUT", "Chromium CDP startup timed out"));
            }
            sleep(Duration::from_millis(50)).await;
        }
        if !owned(state).await? {
            return Err(ReachError::new(
                "BUSY",
                "CDP endpoint does not belong to the dedicated profile",
            ));
        }
        if headed {
            save_json(
                &root.join("login-pin.json"),
                &json!({"pinned":true,"url":url}),
            )?;
        }
        save_json(
            &root.join("watcher-ready.json"),
            &json!({"launch_id":token,"transport":"pipe"}),
        )?;
        tokio::select! {
            result=watch_loop(state,WatchConfig::default())=>result,
            result=child.wait()=>{result.map_err(ReachError::from)?;Ok(())}
        }
    }
    .await;
    connection.close().await;
    if tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    let _ = std::fs::remove_file(root.join("watcher-ready.json"));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bad_readiness_token_fails_before_launch() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            run(d.path(), false, "about:blank", "../escape")
                .await
                .unwrap_err()
                .code,
            "BAD_INPUT"
        );
    }
    #[test]
    fn child_pipe_fixture() {
        if std::env::var_os("ARANA_PIPE_FIXTURE").is_none() {
            return;
        }
        use std::io::{Read, Write};
        let mut input = unsafe { std::fs::File::from_raw_fd(3) };
        let mut output = unsafe { std::fs::File::from_raw_fd(4) };
        let mut bytes = vec![];
        loop {
            let mut b = [0];
            input.read_exact(&mut b).unwrap();
            if b[0] == 0 {
                break;
            }
            bytes.push(b[0]);
        }
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let response = json!({"id":request["id"],"result":{"owned_by_watcher":true}}).to_string();
        output.write_all(response.as_bytes()).unwrap();
        output.write_all(&[0]).unwrap();
    }
    #[tokio::test]
    async fn fd_three_and_four_carry_real_child_commands_and_replies() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "watcher_launch::tests::child_pipe_fixture",
                "--nocapture",
            ])
            .env("ARANA_PIPE_FIXTURE", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let connection = child_pipe(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        drop(command);
        assert_eq!(
            connection
                .call("Browser.getVersion", json!({}))
                .await
                .unwrap()["owned_by_watcher"],
            true
        );
        connection.close().await;
        assert!(child.wait().await.unwrap().success());
    }
}
