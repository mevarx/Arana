use reach_browser::{prepare_opencli, reuse_site};
use std::time::Duration;

#[tokio::test]
async fn prepare_opencli_reports_setup_without_chrome_or_opencli() {
    let state = tempfile::tempdir().unwrap();
    let error = prepare_opencli(state.path(), Duration::from_millis(200))
        .await
        .unwrap_err();
    assert_eq!(error.code, "NEED_SETUP");
}

#[tokio::test]
async fn reuse_site_requires_an_explicit_connected_source() {
    let state = tempfile::tempdir().unwrap();
    let error = reuse_site(
        state.path(),
        &["example.test".into()],
        Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "NEED_SETUP");
}
