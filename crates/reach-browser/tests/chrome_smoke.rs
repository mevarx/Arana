use reach_browser::manage;
use std::net::TcpListener;

/// Opt-in integration smoke against the installed Chrome, isolated to an owned temp profile.
#[tokio::test]
#[ignore = "real installed Chromium, isolated owned profile"]
async fn dedicated_chrome_cdp_setup_status_stop_smoke() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("allocate an unused local port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    std::env::set_var("ARANA_BROWSER_PORT", port.to_string());
    let state = tempfile::tempdir().unwrap();
    let args: Vec<String> = vec![];
    let started = manage("setup", &args, state.path())
        .await
        .expect("launch dedicated Chrome");
    let import_file = state.path().join("cookies.json");
    reach_core::state::atomic_write(&import_file,br#"{"cookies":[{"name":"m3_smoke","value":"ok","domain":"example.com","path":"/","secure":false,"httpOnly":true,"sameSite":"Lax","expires":4102444800}]}"#).unwrap();
    let import_args = vec![import_file.to_string_lossy().to_string()];
    let imported = manage("import", &import_args, state.path()).await;
    let domain_args = vec!["example.com".to_string()];
    let after_import = manage("export", &domain_args, state.path()).await;
    let expired = manage("expire", &domain_args, state.path()).await;
    let after_expire = manage("export", &domain_args, state.path()).await;
    reach_browser::touch(state.path()).unwrap();
    reach_core::state::atomic_write(&state.path().join("browser/login-pin.json"), b"pin").unwrap();
    let watcher_state = state.path().to_path_buf();
    let watcher = tokio::spawn(async move {
        reach_browser::watch_with_config(
            &watcher_state,
            reach_browser::WatchConfig {
                idle_after: std::time::Duration::ZERO,
                poll_every: std::time::Duration::from_millis(20),
            },
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let pinned_status = manage("status", &args, state.path()).await;
    let _ = std::fs::remove_file(state.path().join("browser/login-pin.json"));
    let watched = tokio::time::timeout(std::time::Duration::from_secs(20), watcher).await;
    if watched.is_err() {
        let _ = manage("stop", &args, state.path()).await;
        panic!("idle watcher did not finish");
    }
    let watch_result = watched.unwrap().unwrap();
    let idle_status = manage("status", &args, state.path()).await;
    let stopped = manage("stop", &args, state.path()).await;
    assert_eq!(started["running"], true);
    assert_eq!(started["owned"], true);
    assert!(
        pinned_status.unwrap()["running"] == true,
        "login pin must protect an idle browser"
    );
    assert!(watch_result.is_ok());
    assert!(
        idle_status.unwrap()["running"] == false,
        "idle watcher must close the unpinned owned browser"
    );
    assert_eq!(stopped.unwrap()["stopped"], true);
    assert_eq!(imported.unwrap()["imported"], 1);
    assert!(after_import.unwrap()["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "m3_smoke"));
    assert_eq!(expired.unwrap()["expired"], 1);
    assert!(!after_expire.unwrap()["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "m3_smoke"));
}
