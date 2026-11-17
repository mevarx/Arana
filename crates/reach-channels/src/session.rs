//! Private per-tool session homes (port of `runtime/sessions.py`).
//!
//! Every external CLI runs with `HOME` (and the Windows profile variables)
//! pointed at `state/sessions/<tool>/`, so its login never touches the user's
//! real profile. New directories are recorded in `state.json`; an existing
//! directory filled by someone else is left untouched and reported instead of
//! adopted.

use crate::Result;
use reach_core::{
    state::{self, Store},
    ReachError,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The credential file a tool keeps inside its session home, and the JSON
/// field that must be non-empty for the login to count.
pub struct Credential {
    pub path: PathBuf,
    pub field: &'static str,
}

fn validate_tool(tool: &str) -> Result<()> {
    if tool.is_empty()
        || !tool
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "session tool must be one plain tool identifier",
        ));
    }
    Ok(())
}

/// `state/sessions/<tool>`, created private and recorded as arana-owned.
pub fn session_dir(tool: &str, state_dir: &Path) -> Result<PathBuf> {
    validate_tool(tool)?;
    let store = Store::new(state_dir);
    let ledger = store.load()?;
    let relative = format!("sessions/{tool}");
    let record = &ledger["owned_paths"][&relative];
    let owned =
        record["owner"] == "arana" && record["scope"] == "session" && record["kind"] == "directory";
    let path = state_dir.join("sessions").join(tool);
    if path.exists() && !owned && !is_empty_dir(&path) {
        return Err(ReachError::new(
            "NEED_SETUP",
            "Unrecorded files occupy the native session directory; left untouched",
        ));
    }
    state::private_dir(&path)?;
    store.mutate(|data| {
        data["owned_paths"][&relative] = json!({
            "owner": "arana",
            "scope": "session",
            "kind": "directory",
        });
    })?;
    Ok(path)
}

fn is_empty_dir(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

/// Environment overrides that keep a tool's login and config inside its
/// session home. `APPDATA`/`LOCALAPPDATA` are Windows config/cache roots; a
/// child must not fall back to the user's real profile there either.
pub fn session_env(tool: &str, state_dir: &Path) -> Result<Vec<(String, String)>> {
    let home = session_dir(tool, state_dir)?;
    let home = home.to_string_lossy().into_owned();
    Ok([
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ]
    .iter()
    .map(|key| ((*key).to_string(), home.clone()))
    .collect())
}

/// Credential file and required field, for `login`/`logout` bookkeeping.
/// `None` when the tool authenticates through the browser instead of a file.
pub fn credential(tool: &str, state_dir: &Path) -> Result<Option<Credential>> {
    validate_tool(tool)?;
    let home = state_dir.join("sessions").join(tool);
    Ok(match tool {
        "bili-cli" => Some(Credential {
            path: home.join(".bilibili-cli").join("credential.json"),
            field: "sessdata",
        }),
        _ => None,
    })
}

/// Whether saved login state exists. bili-cli is field-aware: a QR flow can
/// exit successfully and still save an empty credential.
pub fn has_login_state(tool: &str, state_dir: &Path) -> Result<bool> {
    validate_tool(tool)?;
    if tool == "bili-cli" {
        let Some(credential) = credential(tool, state_dir)? else {
            return Ok(false);
        };
        state::ensure_safe(&credential.path)?;
        match std::fs::symlink_metadata(&credential.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let text = state::read_private(&credential.path, 128 * 1024)?;
        let unknown = || {
            ReachError::new(
                "UPSTREAM_BROKEN",
                "Saved login state is unreadable; authentication is unknown",
            )
        };
        let value: Value = serde_json::from_str(&text).map_err(|_| unknown())?;
        if !value.is_object() {
            return Err(unknown());
        }
        return match value.get(credential.field) {
            None => Ok(false),
            Some(Value::String(token)) => Ok(!token.trim().is_empty()),
            _ => Err(unknown()),
        };
    }
    Err(ReachError::new(
        "NEED_SETUP",
        "This tool needs an explicit login probe; session files are not proof of authentication",
    ))
}

/// Delete a session home only when arana recorded ownership of it.
///
/// The whole home is removed, not per-file: it is a fake HOME that arana
/// itself created, so every file inside it — credentials the
/// child tool wrote included — exists only because arana pointed the tool at
/// this directory. That wholesale clear is what makes logout actually revoke
/// credentials; per-file ownership would leave them behind. Ownership is
/// verified before the call, the path is symlink-guarded, and a pre-existing
/// unowned directory is never adopted (see `session_dir`).
pub fn clear_session(tool: &str, state_dir: &Path) -> Result<bool> {
    validate_tool(tool)?;
    let store = Store::new(state_dir);
    let ledger = store.load()?;
    let relative = format!("sessions/{tool}");
    let record = &ledger["owned_paths"][&relative];
    let owned =
        record["owner"] == "arana" && record["scope"] == "session" && record["kind"] == "directory";
    let root = state_dir.join("sessions").join(tool);
    state::ensure_safe(&root)?;
    if !owned || !root.exists() {
        return Ok(false);
    }
    state::ensure_safe(&root)?;
    std::fs::remove_dir_all(&root)?;
    store.mutate(|data| {
        if let Some(paths) = data["owned_paths"].as_object_mut() {
            paths.remove(&relative);
        }
    })?;
    Ok(true)
}
