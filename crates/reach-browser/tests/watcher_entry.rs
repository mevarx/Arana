#[tokio::test]
async fn hidden_watcher_validates_launch_mode_before_browser_start() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().to_string_lossy().into_owned();
    for args in [
        vec![],
        vec![root.clone(), "--launch".into()],
        vec![
            root.clone(),
            "--launch".into(),
            "invalid".into(),
            "about:blank".into(),
            "id".into(),
        ],
    ] {
        let error = reach_browser::watcher_entry(&args).await.unwrap_err();
        assert_eq!(error.code, "BAD_INPUT");
    }
}
