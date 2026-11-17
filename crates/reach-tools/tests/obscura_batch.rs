use reach_tools::obscura_batch::read_many;
use std::time::Duration;

#[tokio::test]
async fn batch_validates_every_url_before_resolving_or_starting_a_tool() {
    let targets = vec![
        "https://example.com".to_owned(),
        "http://127.0.0.1/private".to_owned(),
    ];
    let e = read_many(
        &targets,
        2,
        Duration::from_secs(1),
        std::path::Path::new("absent-batch-state"),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "BAD_INPUT");
}
#[tokio::test]
async fn batch_rejects_unbounded_requests_and_zero_budget() {
    for (targets, workers, budget) in [
        (vec![], 1, Duration::from_secs(1)),
        (
            vec!["https://example.com".to_owned(); 33],
            1,
            Duration::from_secs(1),
        ),
        (
            vec!["https://example.com".to_owned()],
            0,
            Duration::from_secs(1),
        ),
        (
            vec!["https://example.com".to_owned()],
            5,
            Duration::from_secs(1),
        ),
        (vec!["https://example.com".to_owned()], 1, Duration::ZERO),
    ] {
        assert_eq!(
            read_many(
                &targets,
                workers,
                budget,
                std::path::Path::new("absent-batch-state")
            )
            .await
            .unwrap_err()
            .code,
            "BAD_INPUT"
        );
    }
}
