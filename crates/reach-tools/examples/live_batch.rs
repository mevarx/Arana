//! Real, opt-in batch probe. Uses only public fixture pages and private state.
use reach_core::{ReachError, Result};
use reach_tools::{install_obscura, obscura_batch::read_many, remove_obscura};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
#[tokio::main]
async fn main() -> Result<()> {
    let state = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| ReachError::new("BAD_INPUT", "provide an isolated state directory"))?,
    );
    reach_core::state::private_dir(&state)?;
    let state = std::fs::canonicalize(state)?;
    install_obscura(&state).await?;
    let urls = vec![
        "https://example.com/".to_owned(),
        "https://example.org/".to_owned(),
    ];
    let result = read_many(&urls, 2, Duration::from_secs(60), &state).await;
    let removed = remove_obscura(&state)?;
    let items = result?.into_iter().collect::<Result<Vec<_>>>()?;
    if items.len() != urls.len()
        || items
            .iter()
            .any(|v| v["text"].as_str().is_none_or(str::is_empty))
    {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "batch probe returned missing content",
        ));
    }
    println!(
        "{}",
        json!({"passed":true,"backend":"obscura scrape","real_worker_processes":true,"items":items,"remove":removed})
    );
    Ok(())
}
