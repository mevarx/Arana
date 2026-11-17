//! Live driver for the obscura backend: `install`, `version`, `read`,
//! `remove` against a real state directory. Used for live evidence and
//! operator debugging.
//!
//! ```text
//! cargo run -p reach-tools --example live_obscura -- install <state_dir>
//! cargo run -p reach-tools --example live_obscura -- read <state_dir> <url> [timeout_s]
//! cargo run -p reach-tools --example live_obscura -- remove <state_dir>
//! ```

use std::path::PathBuf;
use std::time::Duration;

fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

#[tokio::main]
async fn main() -> reach_core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("help");
    let state_dir: PathBuf = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("arana-live"));
    match command {
        "install" => {
            let record = reach_tools::install_obscura(&state_dir).await?;
            print_json(&record);
            Ok(())
        }
        "read" => {
            let url = args
                .get(3)
                .map(String::as_str)
                .unwrap_or("https://example.com");
            let timeout = args
                .get(4)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            let items =
                reach_tools::obscura_read(url, Duration::from_secs(timeout), &state_dir).await?;
            for item in items {
                print_json(&item);
            }
            Ok(())
        }
        "remove" => {
            let summary = reach_tools::remove_obscura(&state_dir)?;
            print_json(&summary);
            Ok(())
        }
        _ => {
            eprintln!("usage: live_obscura install|read|remove <state_dir> [url] [timeout_s]");
            std::process::exit(2);
        }
    }
}
