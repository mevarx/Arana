use serde_json::json;
use std::{path::Path, time::Duration};
#[test]
fn process_backend_future_layout_is_bounded_for_windows_main_stack() {
    let params = json!({"target":"https://example.com"});
    let path = Path::new(".");
    let budget = Duration::from_secs(3);
    let runner = reach_tools::runner::run_captured(reach_tools::runner::CommandSpec::new(
        "missing", "probe",
    ));
    let obscura = reach_tools::obscura_read("https://example.com", budget, path);
    let media = reach_channels::media::execute("youtube", "read", "yt-dlp", &params, budget, path);
    let execute = reach_channels::execute("web", "read", "obscura", &params, budget, path);
    println!(
        "runner={} obscura={} media={} execute={}",
        std::mem::size_of_val(&runner),
        std::mem::size_of_val(&obscura),
        std::mem::size_of_val(&media),
        std::mem::size_of_val(&execute)
    );
    assert!(
        std::mem::size_of_val(&execute) < 8 * 1024,
        "box heavy branch futures rather than overflowing the Windows main stack"
    );
}
