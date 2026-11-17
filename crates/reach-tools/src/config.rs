//! Config access: `state_dir/config.yaml` with environment fallback
//! (upstream `Config.get`: file first, then `KEY.to_uppercase()` env var).

use reach_core::{ReachError, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Maximum config.yaml size (upstream `_MAX_CONFIG_BYTES`).
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Read `state_dir/config.yaml` as a JSON value. Missing file → `{}`.
/// Reads never create files or directories, follow no symlinks and are
/// size-capped.
pub fn read_config(state_dir: &Path) -> Result<Value> {
    let path = state_dir.join("config.yaml");
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = reach_core::state::read_private(&path, MAX_CONFIG_BYTES)?;
    let parsed: serde_json::Value = serde_yaml::from_str::<serde_yaml::Value>(&text)
        .map_err(|e| ReachError::new("BAD_INPUT", format!("config.yaml is not valid YAML: {e}")))
        .and_then(|v| {
            serde_json::to_value(v).map_err(|e| {
                ReachError::new(
                    "BAD_INPUT",
                    format!("config.yaml is not representable: {e}"),
                )
            })
        })?;
    if !parsed.is_object() {
        return Err(ReachError::new(
            "BAD_INPUT",
            "config.yaml top level must be a mapping",
        ));
    }
    Ok(parsed)
}

/// Get a config value: config.yaml first, then the uppercase env var.
pub fn get(state_dir: &Path, key: &str) -> Option<String> {
    if let Ok(value) = read_config(state_dir) {
        if let Some(found) = value.get(key).and_then(Value::as_str) {
            if !found.is_empty() {
                return Some(found.to_string());
            }
        }
    }
    std::env::var(key.to_uppercase())
        .ok()
        .filter(|v| !v.is_empty())
}
