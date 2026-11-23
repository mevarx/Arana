//! Structured extraction for rendered pages.
//!
//! This is an additive, read-only capability adapted from Scrapling's
//! `Selector`/`Convertor` pipeline (BSD-3-Clause, commit `8864137`):
//!
//! - CSS selection over the parsed document (`Selector.css`);
//! - hidden-content removal before selection: inline `display:none`,
//!   `visibility:hidden`, zero `opacity`/`font-size`/`height`/`width`,
//!   `aria-hidden`, `hidden` and `<template>` (`Convertor._sanitize_for_ai`);
//! - zero-width and control-character scrubbing (`_sanitize_for_ai`);
//! - adaptive relocation: a saved element fingerprint is scored against every
//!   element when the original selector returns nothing (`Selector.relocate`).
//!
//! The fingerprint is stored under `state_dir/extract/<sha256>.json`, keyed by
//! the fragment-free URL and the selector. Text similarity uses a character
//! bigram Dice coefficient instead of Python's `difflib.SequenceMatcher`; the
//! scoring weights and the 40% acceptance threshold are preserved, and the
//! plausible elements within 80% of the best score are returned so a selector
//! that matched a list still yields the list. A relocation also needs an
//! identity signal (class, id, text, or tag+parent), not just structure.
//!
//! This module never fetches; the caller supplies rendered HTML.

use crate::Result;
use ego_tree::NodeRef;
use reach_core::{state, ReachError};
use scraper::{ElementRef, Html, Node, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Minimum relocation score, in percent, copied from Scrapling's default.
pub const RELOCATION_THRESHOLD: f64 = 40.0;
/// Characters kept per item before the item is marked truncated.
pub const MAX_ITEM_CHARS: usize = 200_000;
/// Readable text stored in a fingerprint, bounded like Scrapling's short samples.
const FINGERPRINT_TEXT_CHARS: usize = 200;
/// Hard stop for pathological nesting; deeper nodes are not read.
const MAX_TREE_DEPTH: usize = 512;
/// Hard stop for relocation scans on very large documents.
const MAX_SCAN_ELEMENTS: usize = 20_000;

/// Elements that never contribute readable text.
fn is_noise(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "noscript" | "svg" | "template")
}

fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Normalize CR/CRLF/form-feed the way CSS does before declaration parsing.
fn normalize_css_newlines(style: &str) -> String {
    style.replace("\r\n", "\n").replace(['\r', '\u{c}'], "\n")
}

/// Replace comments, strings and unquoted `url(...)` tokens so their contents
/// cannot be mistaken for declarations. Mirrors Scrapling's `_STYLE_TOKEN_PATTERN`
/// without needing look-around.
fn blank_css_tokens(style: &str) -> String {
    let chars: Vec<char> = style.chars().collect();
    let mut out = String::with_capacity(style.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '\\' {
            out.push(current);
            if let Some(next) = chars.get(index + 1) {
                out.push(*next);
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if current == '/' && chars.get(index + 1) == Some(&'*') {
            out.push(' ');
            index += 2;
            while index < chars.len() {
                if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if current == '"' || current == '\'' {
            out.push('"');
            out.push('"');
            index += 1;
            while index < chars.len() {
                let value = chars[index];
                if value == '\\' {
                    index += 2;
                    continue;
                }
                index += 1;
                if value == current || value == '\n' {
                    break;
                }
            }
            continue;
        }
        if matches!(current, 'u' | 'U') && starts_unquoted_url(&chars, index) {
            out.push('"');
            out.push('"');
            index += 4;
            while index < chars.len() {
                let value = chars[index];
                if value == '\\' {
                    index += 2;
                    continue;
                }
                index += 1;
                if value == ')' {
                    break;
                }
            }
            continue;
        }
        out.push(current);
        index += 1;
    }
    out
}

/// Is `url(` starting at `index`, unquoted, and not part of a longer identifier?
fn starts_unquoted_url(chars: &[char], index: usize) -> bool {
    let preceded_by_word = index > 0
        && (chars[index - 1].is_ascii_alphanumeric()
            || chars[index - 1] == '_'
            || chars[index - 1] == '-');
    if preceded_by_word {
        return false;
    }
    let rest: String = chars[index..]
        .iter()
        .take(4)
        .map(char::to_ascii_lowercase)
        .collect();
    if rest != "url(" {
        return false;
    }
    // CSS allows whitespace before an unquoted url; a quoted one is handled by
    // the string scanner instead.
    let mut probe = index + 4;
    while matches!(chars.get(probe), Some(' ' | '\t' | '\n')) {
        probe += 1;
    }
    !matches!(chars.get(probe), Some('"' | '\''))
}

static ZERO_VALUE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[+-]?(?:0+(?:\.0+)?|\.0+)(?:e[+-]?[0-9]+)?[a-z%]*$")
        .expect("zero value regex")
});

/// True when a declaration value is exactly zero (optionally with a unit).
fn is_zero_value(value: &str) -> bool {
    ZERO_VALUE.is_match(value)
}

/// Port of Scrapling's `_is_hidden_element` for inline styles.
fn style_hides(style: &str) -> bool {
    let normalized = normalize_css_newlines(style);
    let blanked = blank_css_tokens(&normalized);
    for declaration in blanked.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let lowered = value.trim().to_ascii_lowercase();
        let value = lowered
            .strip_suffix("!important")
            .unwrap_or(&lowered)
            .trim();
        if (property == "display" && value == "none")
            || (property == "visibility" && value == "hidden")
        {
            return true;
        }
        if matches!(
            property.as_str(),
            "opacity" | "font-size" | "height" | "width" | "max-height" | "max-width"
        ) && is_zero_value(value)
        {
            return true;
        }
    }
    false
}

fn hidden_element(element: &scraper::node::Element) -> bool {
    if element.name() == "template" {
        return true;
    }
    if element.attr("hidden").is_some() {
        return true;
    }
    if element
        .attr("aria-hidden")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    element.attr("style").is_some_and(style_hides)
}

/// Skip a node when it or any ancestor is noise or hidden from readers.
fn excluded(element: &ElementRef, keep_hidden: bool) -> bool {
    let mut current = Some(**element);
    while let Some(node) = current {
        if let Some(element) = ElementRef::wrap(node) {
            if is_noise(element.value().name()) {
                return true;
            }
            if !keep_hidden && hidden_element(element.value()) {
                return true;
            }
        }
        current = node.parent();
    }
    false
}

fn scrub(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !matches!(
                *character,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{2060}' | '\u{180e}'
            ) && !matches!(
                *character as u32,
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f
            )
        })
        .collect()
}

fn push_line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn walk(
    node: NodeRef<'_, Node>,
    out: &mut String,
    keep_hidden: bool,
    byte_budget: Option<usize>,
    depth: usize,
) {
    if depth > MAX_TREE_DEPTH || byte_budget.is_some_and(|budget| out.len() >= budget) {
        return;
    }
    match node.value() {
        Node::Text(text) => {
            if let Some(budget) = byte_budget {
                let remaining = budget.saturating_sub(out.len());
                if remaining == 0 {
                    return;
                }
                let mut end = remaining.min(text.text.len());
                while end > 0 && !text.text.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&text.text[..end]);
            } else {
                out.push_str(&text.text);
            }
        }
        Node::Element(element) => {
            let tag = element.name();
            if is_noise(tag) {
                return;
            }
            if !keep_hidden && hidden_element(element) {
                return;
            }
            let block = is_block(tag);
            if block {
                push_line_break(out);
            }
            for child in node.children() {
                walk(child, out, keep_hidden, byte_budget, depth + 1);
            }
            if block {
                push_line_break(out);
            }
        }
        _ => {
            for child in node.children() {
                walk(child, out, keep_hidden, byte_budget, depth + 1);
            }
        }
    }
}

/// Normalize whitespace into one sentence per line, collapsing blank runs.
fn normalize_text(text: &str) -> String {
    let scrubbed = scrub(text);
    let mut lines: Vec<String> = Vec::new();
    for line in scrubbed
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
    {
        let cleaned = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if cleaned.is_empty() {
            if lines.last().is_some_and(|last| !last.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(cleaned);
        }
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Readable text of an element, skipping noise and hidden subtrees.
pub fn element_text(element: &ElementRef, keep_hidden: bool) -> String {
    let mut out = String::new();
    walk(**element, &mut out, keep_hidden, None, 0);
    normalize_text(&out)
}

/// A bounded readable sample, used by fingerprints. The byte budget is four
/// times the character limit so any UTF-8 code point fits; the result is
/// truncated to `limit` characters afterwards.
fn text_sample(element: &ElementRef, keep_hidden: bool, limit: usize) -> String {
    let mut out = String::new();
    walk(
        **element,
        &mut out,
        keep_hidden,
        Some(limit.saturating_mul(4)),
        0,
    );
    normalize_text(&out).chars().take(limit).collect()
}

fn cap_text(text: String) -> (String, bool) {
    if text.chars().count() <= MAX_ITEM_CHARS {
        return (text, false);
    }
    (text.chars().take(MAX_ITEM_CHARS).collect(), true)
}

fn attributes(element: &ElementRef) -> Map<String, Value> {
    element
        .value()
        .attrs()
        .filter_map(|(name, value)| {
            let cleaned = scrub(value);
            let value = cleaned.trim();
            if value.is_empty() {
                None
            } else {
                Some((name.to_string(), json!(value)))
            }
        })
        .collect()
}

fn element_path(element: &ElementRef) -> Vec<String> {
    let mut names = vec![element.value().name().to_string()];
    let mut current = element.parent();
    while let Some(node) = current {
        if let Some(parent) = ElementRef::wrap(node) {
            names.push(parent.value().name().to_string());
        }
        current = node.parent();
    }
    names.reverse();
    names
}

fn parent_details(
    element: &ElementRef,
    keep_hidden: bool,
) -> (String, Map<String, Value>, String, Vec<String>) {
    let Some(parent) = element.parent().and_then(ElementRef::wrap) else {
        return (String::new(), Map::new(), String::new(), Vec::new());
    };
    let siblings = parent
        .child_elements()
        .take(33)
        .filter(|sibling| sibling.id() != element.id())
        .take(32)
        .map(|sibling| sibling.value().name().to_string())
        .collect();
    (
        parent.value().name().to_string(),
        attributes(&parent),
        text_sample(&parent, keep_hidden, FINGERPRINT_TEXT_CHARS),
        siblings,
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Fingerprint {
    tag: String,
    attributes: Map<String, Value>,
    text: String,
    path: Vec<String>,
    #[serde(default)]
    parent_name: String,
    #[serde(default)]
    parent_attribs: Map<String, Value>,
    #[serde(default)]
    parent_text: String,
    #[serde(default)]
    siblings: Vec<String>,
}

fn fingerprint(element: &ElementRef, keep_hidden: bool) -> Fingerprint {
    let (parent_name, parent_attribs, parent_text, siblings) = parent_details(element, keep_hidden);
    Fingerprint {
        tag: element.value().name().to_string(),
        attributes: attributes(element),
        text: text_sample(element, keep_hidden, FINGERPRINT_TEXT_CHARS),
        path: element_path(element),
        parent_name,
        parent_attribs,
        parent_text,
        siblings,
    }
}

/// Character-bigram Dice coefficient. This replaces Python's
/// `difflib.SequenceMatcher` for free text while keeping the 0..1 range.
fn string_similarity(left: &str, right: &str) -> f64 {
    if left == right {
        return 1.0;
    }
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    if left_chars.is_empty() || right_chars.is_empty() {
        return 0.0;
    }
    if left_chars.len() < 2 || right_chars.len() < 2 {
        return 0.0;
    }
    let bigrams = |chars: &[char]| {
        let mut counts: HashMap<(char, char), usize> = HashMap::new();
        for pair in chars.windows(2) {
            *counts.entry((pair[0], pair[1])).or_insert(0) += 1;
        }
        counts
    };
    let left_counts = bigrams(&left_chars);
    let right_counts = bigrams(&right_chars);
    let shared: usize = left_counts
        .iter()
        .map(|(pair, count)| (*count).min(*right_counts.get(pair).unwrap_or(&0)))
        .sum();
    let total = (left_chars.len() - 1) + (right_chars.len() - 1);
    (2.0 * shared as f64) / total as f64
}

/// Longest-common-subsequence ratio for ordered lists (paths, sibling tags).
fn sequence_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left.len() > 512 || right.len() > 512 {
        return if left == right { 1.0 } else { 0.0 };
    }
    let mut previous = vec![0usize; right.len() + 1];
    let mut current = vec![0usize; right.len() + 1];
    for left_item in left {
        for (index, right_item) in right.iter().enumerate() {
            current[index + 1] = if left_item == right_item {
                previous[index] + 1
            } else {
                previous[index + 1].max(current[index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }
    let longest = previous[right.len()];
    (2.0 * longest as f64) / (left.len() + right.len()) as f64
}

fn dict_similarity(left: &Map<String, Value>, right: &Map<String, Value>) -> f64 {
    let left_keys: Vec<String> = left.keys().cloned().collect();
    let right_keys: Vec<String> = right.keys().cloned().collect();
    let left_values: Vec<String> = left
        .values()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect();
    let right_values: Vec<String> = right
        .values()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect();
    sequence_similarity(&left_keys, &right_keys) * 0.5
        + sequence_similarity(&left_values, &right_values) * 0.5
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn class_tokens(fingerprint: &Fingerprint) -> HashSet<String> {
    fingerprint
        .attributes
        .get("class")
        .and_then(Value::as_str)
        .map(|value| {
            value
                .split(|character: char| {
                    character.is_whitespace() || character == '-' || character == '_'
                })
                .filter(|token| !token.is_empty())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

/// A high structural score alone can match an unrelated generic container
/// (for example a text-less `<div>` chain on another site). Require one
/// identity signal before accepting a relocation: a shared class token, the
/// same id, similar text, or — when the fingerprint carries no class, id or
/// text — the same tag and parent tag. This is stricter than Scrapling's score
/// threshold and prevents a plausible-looking wrong element.
fn plausible(original: &Fingerprint, candidate: &Fingerprint) -> bool {
    let original_id = original.attributes.get("id").and_then(Value::as_str);
    let candidate_id = candidate.attributes.get("id").and_then(Value::as_str);
    if let Some(id) = original_id.filter(|value| !value.is_empty()) {
        if candidate_id == Some(id) {
            return true;
        }
    }

    let text_similarity = if original.text.is_empty() {
        0.0
    } else {
        string_similarity(&original.text, &candidate.text)
    };
    if text_similarity >= 0.6 {
        return true;
    }

    let original_classes = class_tokens(original);
    let candidate_classes = class_tokens(candidate);
    if !original_classes.is_empty() {
        if original_classes
            .intersection(&candidate_classes)
            .next()
            .is_some()
        {
            return true;
        }
        // A renamed list such as `product-card` -> `card product` keeps most
        // of the class value even when no token is identical.
        let original_value = original
            .attributes
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let candidate_value = candidate
            .attributes
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return !candidate_value.is_empty()
            && string_similarity(original_value, candidate_value) >= 0.5;
    }
    if original_id.is_some_and(|value| !value.is_empty()) {
        return false;
    }
    candidate.tag == original.tag && candidate.parent_name == original.parent_name
}

/// Scrapling's weighted relocation score, in percent.
fn similarity_score(original: &Fingerprint, candidate: &Fingerprint) -> f64 {
    let mut score = 0.0;
    let mut checks = 0.0;

    score += if original.tag == candidate.tag {
        1.0
    } else {
        0.0
    };
    checks += 1.0;

    if !original.text.is_empty() {
        score += string_similarity(&original.text, &candidate.text);
        checks += 1.0;
    }

    score += dict_similarity(&original.attributes, &candidate.attributes);
    checks += 1.0;

    for key in ["class", "id", "href", "src"] {
        if let Some(value) = original
            .attributes
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let candidate_value = candidate
                .attributes
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default();
            score += string_similarity(value, candidate_value);
            checks += 1.0;
        }
    }

    score += sequence_similarity(&original.path, &candidate.path);
    checks += 1.0;

    if !original.parent_name.is_empty() && !candidate.parent_name.is_empty() {
        score += string_similarity(&original.parent_name, &candidate.parent_name);
        checks += 1.0;
        score += dict_similarity(&original.parent_attribs, &candidate.parent_attribs);
        checks += 1.0;
        if !original.parent_text.is_empty() {
            score += string_similarity(&original.parent_text, &candidate.parent_text);
            checks += 1.0;
        }
    }

    if !original.siblings.is_empty() {
        score += sequence_similarity(&original.siblings, &candidate.siblings);
        checks += 1.0;
    }

    round2(score / checks * 100.0)
}

/// Relocated elements plus whether the scan hit its element cap.
struct Relocation<'a> {
    matches: Vec<(ElementRef<'a>, f64)>,
    truncated: bool,
}

impl<'a> Relocation<'a> {
    fn empty() -> Self {
        Self {
            matches: Vec::new(),
            truncated: false,
        }
    }
}

/// Score every element and return the plausible near-best band when the best
/// score reaches the 40% threshold. Mirrors `Selector.relocate`, with a scan cap
/// and an identity gate.
fn relocate<'a>(
    document: &'a Html,
    original: &Fingerprint,
    limit: usize,
    keep_hidden: bool,
) -> Relocation<'a> {
    let all = match Selector::parse("*") {
        Ok(selector) => selector,
        Err(_) => return Relocation::empty(),
    };
    let mut scored: Vec<(ElementRef<'a>, f64)> = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;
    for element in document.select(&all) {
        scanned += 1;
        if scanned > MAX_SCAN_ELEMENTS {
            truncated = true;
            break;
        }
        if excluded(&element, keep_hidden) {
            continue;
        }
        let candidate = fingerprint(&element, keep_hidden);
        if !plausible(original, &candidate) {
            continue;
        }
        scored.push((element, similarity_score(original, &candidate)));
    }
    let Some(best) = scored
        .iter()
        .map(|(_, score)| *score)
        .fold(None, |best: Option<f64>, score| {
            Some(best.map_or(score, |value| value.max(score)))
        })
    else {
        return Relocation {
            matches: Vec::new(),
            truncated,
        };
    };
    if best < RELOCATION_THRESHOLD {
        return Relocation {
            matches: Vec::new(),
            truncated,
        };
    }
    // A selector can legitimately match many sibling elements whose text
    // differs, so keep the plausible near-best band instead of only exact ties.
    let cutoff = (best * 0.8).max(RELOCATION_THRESHOLD);
    scored.retain(|(_, score)| *score >= cutoff);
    if scored.len() > limit {
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
    }
    Relocation {
        matches: scored,
        truncated,
    }
}

fn canonical_url(url: &str) -> String {
    url::Url::parse(url)
        .map(|mut parsed| {
            parsed.set_fragment(None);
            parsed.to_string()
        })
        .unwrap_or_else(|_| url.to_string())
}

fn storage_path(state_dir: &Path, url: &str, selector: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{}\n{}", canonical_url(url), selector).as_bytes());
    state_dir.join("extract").join(format!("{digest:x}.json"))
}

fn load_fingerprint(state_dir: &Path, url: &str, selector: &str) -> Option<Fingerprint> {
    let path = storage_path(state_dir, url, selector);
    if !path.exists() {
        return None;
    }
    if state::ensure_safe(&path).is_err() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let document: Value = serde_json::from_str(&text).ok()?;
    if document.get("url").and_then(Value::as_str) != Some(canonical_url(url).as_str()) {
        return None;
    }
    if document.get("selector").and_then(Value::as_str) != Some(selector) {
        return None;
    }
    serde_json::from_value(document.get("fingerprint")?.clone()).ok()
}

fn save_fingerprint(
    state_dir: &Path,
    url: &str,
    selector: &str,
    saved: &Fingerprint,
) -> Result<()> {
    let path = storage_path(state_dir, url, selector);
    let body = json!({
        "version": 1,
        "url": canonical_url(url),
        "selector": selector,
        "saved_at": chrono::Utc::now().to_rfc3339(),
        "fingerprint": saved,
    });
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "could not encode the fingerprint"))?;
    state::atomic_write(&path, &bytes)
}

/// Reject an invalid selector before the page is fetched.
pub fn check_selector(selector: &str) -> Result<()> {
    Selector::parse(selector)
        .map(|_| ())
        .map_err(|_| ReachError::new("BAD_INPUT", "invalid CSS selector"))
}

/// Options for one extraction.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub selector: Option<String>,
    pub attr: Option<String>,
    pub absolute: bool,
    pub limit: usize,
    pub keep_hidden: bool,
    pub adaptive: bool,
}

/// One extraction result, ready for the envelope.
#[derive(Debug)]
pub struct Outcome {
    pub items: Vec<Value>,
    pub matched: usize,
    pub adaptive_score: Option<f64>,
    pub saved: bool,
    pub notes: Vec<String>,
}

fn is_url_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "href" | "src" | "action" | "poster" | "cite" | "formaction" | "data-href" | "data-src"
    )
}

fn resolve_url(base: &str, value: &str) -> Option<String> {
    url::Url::parse(base)
        .ok()
        .and_then(|parsed| parsed.join(value).ok())
        .filter(|joined| {
            matches!(joined.scheme(), "http" | "https")
                && joined.username().is_empty()
                && joined.password().is_none()
        })
        .map(|joined| joined.to_string())
}

#[allow(clippy::too_many_arguments)]
fn item_for(
    element: &ElementRef,
    page_url: &str,
    attr: Option<&str>,
    absolute: bool,
    keep_hidden: bool,
    adaptive_score: Option<f64>,
) -> Option<Value> {
    let (value, html_attr) = match attr {
        Some(name) => {
            let clean = scrub(element.value().attr(name)?);
            let raw = clean.trim();
            if raw.is_empty() {
                return None;
            }
            let resolved = if absolute && is_url_attribute(name) {
                resolve_url(page_url, raw)?
            } else {
                raw.to_string()
            };
            (resolved, Some(name.to_string()))
        }
        None => {
            let text = element_text(element, keep_hidden);
            if text.is_empty() {
                return None;
            }
            (text, None)
        }
    };
    let (value, truncated) = cap_text(value);
    let raw = json!({
        "tag": element.value().name(),
        "attributes": attributes(element),
        "path": element_path(element).join(" > "),
        "attr": html_attr,
        "adaptive_score": adaptive_score,
        "truncated": truncated,
    });
    Some(reach_core::make_item(
        None,
        Some(page_url.to_string()),
        None,
        None,
        Some(value),
        raw,
    ))
}

/// Extract structured content from one rendered page.
pub fn extract(html: &str, page_url: &str, options: &Options, state_dir: &Path) -> Result<Outcome> {
    let document = Html::parse_document(html);
    let limit = options.limit.max(1);
    let mut notes = Vec::new();
    let mut adaptive_score = None;
    let mut saved = false;
    let mut scores = Vec::new();

    let mut selected: Vec<ElementRef<'_>> = Vec::new();
    let matched;

    match options.selector.as_deref() {
        Some(css) => {
            let selector = Selector::parse(css)
                .map_err(|_| ReachError::new("BAD_INPUT", "invalid CSS selector"))?;
            selected = document
                .select(&selector)
                .filter(|element| !excluded(element, options.keep_hidden))
                .collect();
            matched = selected.len();

            if matched == 0 && options.adaptive {
                match load_fingerprint(state_dir, page_url, css) {
                    Some(original) => {
                        let relocated = relocate(&document, &original, limit, options.keep_hidden);
                        if relocated.matches.is_empty() {
                            notes.push(
                                "adaptive relocation found no element at or above 40%".into(),
                            );
                        } else {
                            adaptive_score = relocated
                                .matches
                                .iter()
                                .map(|(_, score)| *score)
                                .reduce(f64::max);
                            scores = relocated.matches.iter().map(|(_, score)| *score).collect();
                            selected = relocated
                                .matches
                                .into_iter()
                                .map(|(element, _)| element)
                                .collect();
                            notes.push(format!(
                                "adaptive relocation matched {} element(s)",
                                selected.len()
                            ));
                        }
                        if relocated.truncated {
                            notes.push(format!(
                                "relocation scanned the first {MAX_SCAN_ELEMENTS} elements"
                            ));
                        }
                    }
                    None => notes.push("no saved fingerprint for this URL and selector".into()),
                }
            } else if matched == 0 {
                notes.push("selector matched no visible elements".into());
            }

            if options.adaptive && matched > 0 {
                match save_fingerprint(
                    state_dir,
                    page_url,
                    css,
                    &fingerprint(&selected[0], options.keep_hidden),
                ) {
                    Ok(()) => saved = true,
                    Err(error) => notes.push(format!("fingerprint not saved: {}", error.message)),
                }
            }
        }
        None => {
            let body = Selector::parse("body").ok();
            let root = body
                .as_ref()
                .and_then(|selector| document.select(selector).next())
                .unwrap_or_else(|| document.root_element());
            selected.push(root);
            matched = 1;
        }
    }

    let attr = options.attr.as_deref();
    let mut items = Vec::new();
    for (index, element) in selected.iter().take(limit).enumerate() {
        if let Some(item) = item_for(
            element,
            page_url,
            attr,
            options.absolute,
            options.keep_hidden,
            scores.get(index).copied(),
        ) {
            items.push(item);
        }
    }

    Ok(Outcome {
        items,
        matched,
        adaptive_score,
        saved,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Html {
        Html::parse_document(html)
    }

    fn select<'a>(document: &'a Html, css: &str) -> Vec<ElementRef<'a>> {
        let selector = Selector::parse(css).expect("selector");
        document.select(&selector).collect()
    }

    #[test]
    fn inline_style_hiding_is_declaration_precise() {
        assert!(style_hides("display:none"));
        assert!(style_hides("display: none !important"));
        assert!(style_hides("visibility:hidden"));
        assert!(style_hides("opacity:0"));
        assert!(style_hides("opacity:0.0"));
        assert!(style_hides("font-size:0px"));
        assert!(style_hides("max-height: 0"));
        assert!(style_hides("width:0%"));
        // Values that merely start with zero, or share a suffix, stay visible.
        assert!(!style_hides("opacity:0.95"));
        assert!(!style_hides("line-height:0.9"));
        assert!(!style_hides("height:100%"));
        // A hiding value inside a comment, string or url() is not a declaration.
        assert!(!style_hides("color:red; /* display:none */"));
        assert!(!style_hides("background:url(display:none.png)"));
        assert!(!style_hides("background:url( display:none.png )"));
        assert!(!style_hides("background:url(\"display:none.png\")"));
        assert!(!style_hides("content:\"display:none\""));
        assert!(!style_hides("display:block;visibility:visible"));
    }

    #[test]
    fn hidden_content_is_removed_before_selection() {
        let html = r#"
            <html><body>
              <div class="price">$42</div>
              <div class="price" style="display:none">$1 (injected)</div>
              <span class="price" aria-hidden="true">$2</span>
              <p hidden>$3</p>
              <template><div class="price">$4</div></template>
              <script>document.write("$5")</script>
            </body></html>
        "#;
        let document = parse(html);
        let visible: Vec<String> = select(&document, ".price")
            .iter()
            .filter(|element| !excluded(element, false))
            .map(|element| element_text(element, false))
            .collect();
        assert_eq!(visible, vec!["$42"]);
    }

    #[test]
    fn text_collection_is_block_aware_and_scrubbed() {
        let html = r#"<body><h1>Title</h1><p>Hello <b>world</b>.</p><ul><li>one</li><li>two</li></ul><p>zero\u{200b}width</p></body>"#;
        let document = parse(html);
        let body = document.root_element();
        let text = element_text(&body, false);
        assert!(text.contains("Title\nHello world.\none\ntwo"), "{text}");
        assert!(!text.contains('\u{200b}'));
    }

    #[test]
    fn pathological_nesting_is_bounded() {
        let html = format!(
            "<body>{}deep text{}</body>",
            "<div>".repeat(MAX_TREE_DEPTH + 16),
            "</div>".repeat(MAX_TREE_DEPTH + 16)
        );
        let document = parse(&html);
        let text = element_text(&document.root_element(), false);
        assert!(!text.contains("deep text"), "{text}");
    }

    #[test]
    fn relocation_finds_a_renamed_class_and_returns_ties() {
        let before = r#"<html><body><div class="product-card"><h2>A</h2><span class="price">$10</span></div><div class="product-card"><h2>B</h2><span class="price">$20</span></div></body></html>"#;
        let after = r#"<html><body><div class="card product"><h2>A</h2><span class="cost">$10</span></div><div class="card product"><h2>B</h2><span class="cost">$20</span></div></body></html>"#;
        let original_doc = parse(before);
        let original_elements = select(&original_doc, ".product-card");
        let original = fingerprint(original_elements.first().expect("product card"), false);
        let after_doc = parse(after);
        let relocated = relocate(&after_doc, &original, 25, false);
        assert_eq!(relocated.matches.len(), 2, "{:?}", relocated.matches);
        assert!(relocated.matches[0].1 >= RELOCATION_THRESHOLD);
    }

    #[test]
    fn relocation_rejects_an_unrelated_page() {
        let before = r#"<html><body><div class="product-card">A</div></body></html>"#;
        let after =
            r#"<html><body><article><h1>Blog</h1><p>Unrelated words.</p></article></body></html>"#;
        let original_doc = parse(before);
        let original_elements = select(&original_doc, ".product-card");
        let original = fingerprint(original_elements.first().expect("product card"), false);
        let after_doc = parse(after);
        assert!(relocate(&after_doc, &original, 25, false)
            .matches
            .is_empty());
    }

    #[test]
    fn relocation_requires_an_identity_signal_not_just_structure() {
        // Same tag chain, same parent, different class and text: the structural
        // score alone can pass 40%, but this is a different element.
        let before = r#"<html><body><div class="quote"><span>A</span></div></body></html>"#;
        let after = r#"<html><body><div class="widget"><span>Unrelated</span></div></body></html>"#;
        let original_doc = parse(before);
        let original_elements = select(&original_doc, ".quote");
        let original = fingerprint(original_elements.first().expect("quote"), false);
        let after_doc = parse(after);
        assert!(relocate(&after_doc, &original, 25, false)
            .matches
            .is_empty());
    }
}
