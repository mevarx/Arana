//! Structured extraction: selection, hidden-content removal, attributes and
//! adaptive relocation against a state directory.

use reach_channels::extract::{check_selector, extract, Options};
use serde_json::Value;

fn options(selector: &str) -> Options {
    Options {
        selector: Some(selector.to_string()),
        limit: 25,
        ..Options::default()
    }
}

fn texts(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn selects_visible_text_and_resolves_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let html = r#"
        <html><body>
          <div class="quote">
            <span class="text">First quote</span>
            <a class="author" href="/authors/ada">Ada</a>
          </div>
          <div class="quote">
            <span class="text">Second quote</span>
            <a class="author" href="https://example.org/bob">Bob</a>
          </div>
          <div class="quote" style="display:none">Hidden injected quote</div>
        </body></html>
    "#;

    let outcome = extract(
        html,
        "https://example.com/quotes",
        &options(".quote"),
        dir.path(),
    )
    .expect("extract");
    assert_eq!(outcome.matched, 2);
    assert_eq!(outcome.items.len(), 2);
    assert_eq!(
        texts(&outcome.items),
        vec!["First quote\nAda", "Second quote\nBob"]
    );
    assert_eq!(outcome.items[0]["raw"]["tag"], "div");
    assert_eq!(outcome.items[0]["raw"]["path"], "html > body > div");

    let mut attribute = options(".quote .author");
    attribute.attr = Some("href".into());
    attribute.absolute = true;
    let outcome = extract(html, "https://example.com/quotes", &attribute, dir.path())
        .expect("attribute extract");
    assert_eq!(
        texts(&outcome.items),
        vec!["https://example.com/authors/ada", "https://example.org/bob"]
    );
}

#[test]
fn adaptive_relocates_after_a_class_change_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let before = r#"
        <html><body>
          <section class="results">
            <div class="product-card"><h2>A</h2><span class="price">$10</span></div>
            <div class="product-card"><h2>B</h2><span class="price">$20</span></div>
          </section>
        </body></html>
    "#;
    let mut opts = options(".product-card");
    opts.adaptive = true;
    let first = extract(before, "https://shop.test/list", &opts, dir.path()).expect("first");
    assert_eq!(first.items.len(), 2);
    assert!(first.saved, "{first:?}");
    let stored = std::fs::read_dir(dir.path().join("extract"))
        .expect("fingerprint dir")
        .count();
    assert_eq!(stored, 1);

    // The same page after a redesign: the class changed, the product blocks did not.
    let after = r#"
        <html><body>
          <section class="results">
            <div class="card item"><h2>A</h2><span class="cost">$10</span></div>
            <div class="card item"><h2>B</h2><span class="cost">$20</span></div>
          </section>
        </body></html>
    "#;
    let second = extract(after, "https://shop.test/list", &opts, dir.path()).expect("second");
    assert_eq!(second.matched, 0, "the old selector must not match");
    assert_eq!(second.items.len(), 2, "{second:?}");
    assert!(second.adaptive_score.is_some_and(|score| score >= 40.0));
    assert_eq!(second.items[0]["raw"]["tag"], "div");
    assert!(second.items[0]["raw"]["adaptive_score"].is_number());
}

#[test]
fn limit_is_applied_while_matched_reports_all() {
    let dir = tempfile::tempdir().unwrap();
    let html = r#"<html><body>
        <p class="row">one</p><p class="row">two</p><p class="row">three</p>
        <p class="row">four</p><p class="row">five</p>
    </body></html>"#;
    let mut opts = options(".row");
    opts.limit = 2;
    let outcome = extract(html, "https://example.com/", &opts, dir.path()).expect("extract");
    assert_eq!(outcome.matched, 5);
    assert_eq!(outcome.items.len(), 2);
}

#[test]
fn without_a_selector_the_page_text_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let html = r#"<html><head><title>Ignored</title><style>.x{color:red}</style></head>
        <body><h1>Heading</h1><p>Body text</p><script>alert(1)</script></body></html>"#;
    let opts = Options::default();
    let outcome = extract(html, "https://example.com/", &opts, dir.path()).expect("extract");
    assert_eq!(outcome.items.len(), 1);
    let text = outcome.items[0]["text"].as_str().unwrap();
    assert!(text.contains("Heading\nBody text"), "{text}");
    assert!(!text.contains("alert"), "{text}");
    assert!(!text.contains("color:red"), "{text}");
}

#[test]
fn keep_hidden_includes_hidden_content_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let html =
        r#"<html><body><p class="row">visible</p><p class="row" hidden>hidden</p></body></html>"#;
    let mut opts = options(".row");
    opts.keep_hidden = true;
    let outcome = extract(html, "https://example.com/", &opts, dir.path()).expect("extract");
    assert_eq!(texts(&outcome.items), vec!["visible", "hidden"]);
}

#[test]
fn adaptive_attributes_exclude_hidden_elements() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options("a.old");
    opts.adaptive = true;
    opts.attr = Some("href".into());
    let url = "https://example.com/";
    assert!(
        extract(
            "<a class='old entry' href='/docs'>Docs</a>",
            url,
            &opts,
            dir.path()
        )
        .unwrap()
        .saved
    );
    let changed = "<div hidden><a class='entry' href='/docs'>Docs</a></div>";
    assert!(extract(changed, url, &opts, dir.path())
        .unwrap()
        .items
        .is_empty());
}

#[test]
fn absolute_links_return_only_http_urls() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options("a");
    opts.attr = Some("href".into());
    opts.absolute = true;
    let html = "<a href='mailto:help@example.com'>Email</a><a href='/docs'>Docs</a>";
    let result = extract(html, "https://example.com/", &opts, dir.path()).unwrap();
    assert_eq!(texts(&result.items), ["https://example.com/docs"]);
}

#[test]
fn invalid_selectors_are_bad_input() {
    let error = check_selector("div[[").expect_err("invalid selector");
    assert_eq!(error.code, "BAD_INPUT");
}
