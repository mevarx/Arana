//! Shared resolution and bounded execution for external channel CLIs.

use crate::Result;
use reach_core::ReachError;
use reach_tools::runner::{self, CommandOutput, CommandSpec};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Resolve one external command: the `ARANA_*_BIN` test/operator override
/// first, then the registry (private install, PATH). The override also covers
/// tools the registry does not list yet, without putting a private copy on
/// `PATH`.
pub fn program(command: &str, env_key: &str, state_dir: &Path) -> Result<PathBuf> {
    if let Ok(path) = std::env::var(env_key) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() && path.exists() {
            return Ok(path);
        }
    }
    reach_tools::registry::resolve(command, state_dir)
        .ok_or_else(|| ReachError::new("NEED_SETUP", format!("{command} is not installed")))
}

/// Run an external tool with a hard deadline and captured streams.
pub async fn run(
    program: &Path,
    args: &[String],
    env: Vec<(String, String)>,
    timeout: Duration,
    label: &str,
) -> Result<CommandOutput> {
    #[cfg(windows)]
    let (program, args) = windows_argv(program, args)?;
    #[cfg(not(windows))]
    let (program, args) = (program.to_path_buf(), args.to_vec());
    let mut spec = CommandSpec::new(program, label);
    spec.args = args;
    spec.timeout = timeout;
    spec.env = env;
    runner::run_captured(spec).await
}

/// npm installs `.cmd` launchers on Windows. Run the JavaScript entry point
/// with Node directly so arguments such as URLs containing `&` survive, and
/// refuse cmd.exe metacharacters when only the shim is available (port of
/// upstream `_windows_argv`).
#[cfg(windows)]
fn windows_argv(program: &Path, args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    use std::sync::LazyLock;
    static SHIM: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#""%dp0%\\(?P<script>[^"\r\n]*?\.(?:cjs|mjs|js))""#)
            .expect("npm shim regex")
    });
    let is_batch = program
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });
    if !is_batch {
        return Ok((program.to_path_buf(), args.to_vec()));
    }
    let shim = std::fs::read_to_string(program).unwrap_or_default();
    if let Some(script) = SHIM
        .captures(&shim)
        .and_then(|captures| captures.name("script"))
    {
        let script = program
            .parent()
            .expect("shim parent")
            .join(script.as_str().replace('\\', std::path::MAIN_SEPARATOR_STR));
        if script.is_file() {
            if let Ok(node) = which::which("node") {
                let mut argv = vec![script.to_string_lossy().into_owned()];
                argv.extend(args.iter().cloned());
                return Ok((node, argv));
            }
        }
    }
    const META: [char; 10] = ['&', '|', '<', '>', '^', '(', ')', '%', '!', '"'];
    if args.iter().any(|arg| {
        arg.chars()
            .any(|c| META.contains(&c) || c == '\r' || c == '\n')
    }) {
        return Err(ReachError::new(
            "BAD_INPUT",
            format!(
                "{} cannot safely receive arguments containing cmd.exe metacharacters",
                program
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("the launcher")
            ),
        ));
    }
    Ok((program.to_path_buf(), args.to_vec()))
}

/// Parse the first JSON object in a tool's stdout (Node warnings and log lines
/// may precede it).
pub fn json_payload(stdout: &str, label: &str) -> Result<Value> {
    let start = stdout.find('{').ok_or_else(|| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!("{label} returned unreadable output"),
        )
    })?;
    serde_json::from_str(&stdout[start..]).map_err(|_| {
        ReachError::new(
            "UPSTREAM_BROKEN",
            format!("{label} returned unreadable output"),
        )
    })
}

/// Classify a non-zero exit that printed no parseable envelope.
pub fn failed_exit(output: &CommandOutput, label: &str) -> ReachError {
    let last = runner::last_line(&output.stderr).trim();
    let message = if last.is_empty() {
        format!("{label} exited {}: no error output", output.code())
    } else {
        format!("{label} exited {}: {last}", output.code())
    };
    ReachError::new("UPSTREAM_BROKEN", message)
}

/// Apply an upstream hint only when the mapped code has no generic advice.
pub fn with_hint(mut error: ReachError, hint: Option<&str>) -> ReachError {
    if let Some(hint) = hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        error.hint = hint.to_string();
    }
    error
}
