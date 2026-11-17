//! Bounded multi-URL reads using the pinned `obscura scrape` worker protocol.
//! Kept as a library API: adding a CLI action would change the frozen catalog.
use crate::{
    registry,
    runner::{self, CommandSpec},
    ssrf,
};
use reach_core::{make_item, ReachError, Result};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
const MAX_URLS: usize = 32;
const MAX_WORKERS: usize = 4;
const MAX_BYTES: usize = 5 * 1024 * 1024;

/// Read up to 32 public URLs with 1–4 workers, preserving input order and
/// individual failures. Validation precedes tool lookup; the timeout and output
/// cap apply to the complete subprocess, not separately to every worker.
pub async fn read_many(
    targets: &[String],
    workers: usize,
    timeout: Duration,
    state: &Path,
) -> Result<Vec<Result<Value>>> {
    let (urls, args) = request(targets, workers, timeout)?;
    let executable = registry::resolve("obscura", state)
        .ok_or_else(|| ReachError::new("NEED_SETUP", "obscura is not installed"))?;
    let mut spec = CommandSpec::new(executable, "obscura scrape");
    spec.args = args;
    spec.timeout = timeout;
    spec.max_output_bytes = MAX_BYTES;
    let output = runner::run_captured(spec).await?;
    if !output.success() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!(
                "obscura scrape exited {}: {}",
                output.code(),
                runner::last_line(&output.stderr)
            ),
        ));
    }
    parse(&output.stdout, &urls)
}
fn request(
    targets: &[String],
    workers: usize,
    timeout: Duration,
) -> Result<(Vec<String>, Vec<String>)> {
    if targets.is_empty()
        || targets.len() > MAX_URLS
        || !(1..=MAX_WORKERS).contains(&workers)
        || timeout.is_zero()
    {
        return Err(ReachError::new(
            "BAD_INPUT",
            "batch requires 1–32 URLs, 1–4 workers and a positive timeout",
        ));
    }
    let urls: Vec<String> = targets
        .iter()
        .map(|url| {
            ssrf::normalize_public_http_url(url).map_err(|_| {
                ReachError::new(
                    "BAD_INPUT",
                    "every batch target must be a public http(s) URL",
                )
            })
        })
        .collect::<Result<_>>()?;
    let mut args = vec![
        "scrape".to_owned(),
        "--quiet".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
        "--concurrency".to_owned(),
        workers.to_string(),
        "--timeout".to_owned(),
        timeout.as_secs().clamp(1, 120).to_string(),
        "--eval".to_owned(),
        "document.body ? document.body.innerText : ''".to_owned(),
        "--".to_owned(),
    ];
    args.extend(urls.iter().cloned());
    Ok((urls, args))
}
fn broken() -> ReachError {
    ReachError::new(
        "UPSTREAM_BROKEN",
        "obscura scrape returned a malformed or mismatched result",
    )
}
fn parse(raw: &str, urls: &[String]) -> Result<Vec<Result<Value>>> {
    let value: Value = serde_json::from_str(raw).map_err(|_| broken())?;
    let rows = value["results"].as_array().ok_or_else(broken)?;
    if rows.len() != urls.len() || value["total_urls"].as_u64() != Some(urls.len() as u64) {
        return Err(broken());
    }
    rows.iter()
        .zip(urls)
        .map(|(row, url)| {
            if let Some(error) = row["error"].as_str() {
                return Ok(Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    format!("obscura scrape: {error}"),
                )));
            }
            if row["url"].as_str() != Some(url.as_str()) {
                return Err(broken());
            }
            let Some(text) = row["eval"]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                return Ok(Err(ReachError::new(
                    "UPSTREAM_BROKEN",
                    "obscura returned no readable content",
                )));
            };
            Ok(Ok(make_item(
                row["title"].as_str().map(str::to_owned),
                Some(url.clone()),
                None,
                None,
                Some(text.to_owned()),
                json!({"backend":"obscura","dump":"text","batch":true}),
            )))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_bounds_workers_and_uses_only_fixed_javascript() {
        let (urls, args) = request(
            &["https://example.com".to_owned()],
            2,
            Duration::from_millis(25),
        )
        .unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(args[0], "scrape");
        assert_eq!(&args[5..8], &["2", "--timeout", "1"]);
        assert!(!args.iter().any(|s| s == "--allow-private-network"));
        assert_eq!(args[10], "--");
    }
    #[test]
    fn ordered_results_keep_per_url_failures() {
        let urls = vec![
            "https://example.com/".to_owned(),
            "https://example.org/".to_owned(),
        ];
        let raw=json!({"total_urls":2,"results":[{"url":urls[0],"title":"Example","eval":" hello "},{"url":urls[1],"error":"request failed"}]}).to_string();
        let results = parse(&raw, &urls).unwrap();
        assert_eq!(results[0].as_ref().unwrap()["text"], "hello");
        assert_eq!(results[1].as_ref().unwrap_err().code, "UPSTREAM_BROKEN");
    }
    #[test]
    fn malformed_counts_and_changed_urls_are_rejected() {
        let urls = vec!["https://example.com/".to_owned()];
        for value in [
            json!({"total_urls":1,"results":[]}),
            json!({"total_urls":2,"results":[{"url":urls[0],"eval":"ok"}]}),
            json!({"total_urls":1,"results":[{"url":"http://127.0.0.1/","eval":"wrong"}]}),
        ] {
            assert_eq!(
                parse(&value.to_string(), &urls).unwrap_err().code,
                "UPSTREAM_BROKEN"
            );
        }
    }
    #[test]
    fn empty_eval_is_an_individual_failure() {
        let urls = vec!["https://example.com/".to_owned()];
        let raw = json!({"total_urls":1,"results":[{"url":urls[0],"eval":"  "}]}).to_string();
        assert_eq!(
            parse(&raw, &urls).unwrap()[0].as_ref().unwrap_err().code,
            "UPSTREAM_BROKEN"
        );
    }
}
