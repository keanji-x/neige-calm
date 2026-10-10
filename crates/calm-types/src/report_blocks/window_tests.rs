//! `window` payloads through the one write-end validator, [`validate_payload`], and the one `src`
//! rule pinned by the fixture the frontend reads too (`test-data/window-src-v1.json`).

use proptest::prelude::*;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{WINDOW_SRC_PATTERN, WINDOW_SRC_PREFIX};
use crate::plugin::is_valid_plugin_id;
use crate::report_blocks::{KIND_WINDOW, validate_payload};

const SRC: &str = "/api/plugins/desktop/ws/apps/chrome/stream";

#[derive(Deserialize)]
struct Cases {
    pattern: String,
    accept: Vec<String>,
    refuse: Vec<String>,
}

fn cases() -> Cases {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/window-src-v1.json"
    )))
    .expect("window-src-v1.json parses")
}

fn accepts(src: &str) -> bool {
    validate_payload(KIND_WINDOW, &json!({ "src": src })).is_ok()
}

#[test]
fn the_published_pattern_is_the_fixture_pattern() {
    assert_eq!(WINDOW_SRC_PATTERN, cases().pattern);
}

#[test]
fn the_validator_agrees_with_every_fixture_case() {
    let cases = cases();
    let wrong: Vec<(&String, bool)> = cases
        .accept
        .iter()
        .map(|src| (src, true))
        .chain(cases.refuse.iter().map(|src| (src, false)))
        .filter(|(src, want)| accepts(src) != *want)
        .collect();
    assert!(
        wrong.is_empty(),
        "validator disagrees (src, want): {wrong:?}"
    );
}

#[test]
fn the_published_pattern_agrees_with_every_fixture_case() {
    let cases = cases();
    let pattern = Regex::new(WINDOW_SRC_PATTERN).expect("the pattern compiles");
    let wrong: Vec<(&String, bool)> = cases
        .accept
        .iter()
        .map(|src| (src, true))
        .chain(cases.refuse.iter().map(|src| (src, false)))
        .filter(|(src, want)| pattern.is_match(src) != *want)
        .collect();
    assert!(wrong.is_empty(), "pattern disagrees (src, want): {wrong:?}");
}

/// The plugin id inlined into the pattern, as its own anchored expression.
fn inlined_plugin_id() -> Regex {
    let after_prefix = WINDOW_SRC_PATTERN
        .strip_prefix(&format!("^{WINDOW_SRC_PREFIX}"))
        .expect("the pattern starts with the prefix");
    let (id, _) = after_prefix
        .split_once("/ws/")
        .expect("the id ends at /ws/");
    Regex::new(&format!("^(?:{id})$")).expect("the id fragment compiles")
}

proptest! {
    #[test]
    fn the_inlined_plugin_id_is_the_manifest_plugin_id_rule(id in "[a-z0-9.A-Z_/-]{0,70}") {
        prop_assert_eq!(inlined_plugin_id().is_match(&id), is_valid_plugin_id(&id), "{}", id);
    }
}

#[test]
fn the_inlined_plugin_id_matches_the_manifest_rule_at_its_edges() {
    let pattern = inlined_plugin_id();
    for id in [
        "ab",
        "a",
        ".a",
        "-a",
        "a.",
        "a-",
        "dev-neige-market",
        &"a".repeat(64),
        &"a".repeat(65),
    ] {
        assert_eq!(pattern.is_match(id), is_valid_plugin_id(id), "{id}");
    }
}

fn refuses(payload: Value, needle: &str) {
    let err = validate_payload(KIND_WINDOW, &payload).expect_err(&payload.to_string());
    assert!(err.contains(needle), "{payload} → {err}");
}

#[test]
fn window_accepts_optional_title_and_height() {
    assert_eq!(
        validate_payload(
            KIND_WINDOW,
            &json!({ "src": SRC, "title": "Chrome", "height": 720 })
        ),
        Ok(())
    );
}

#[test]
fn window_refuses_unknown_fields_and_mistyped_title_or_height() {
    refuses(
        json!({ "src": SRC, "plugin": "desktop" }),
        "plugin: unknown field",
    );
    refuses(json!({}), "src: required");
    refuses(json!({ "src": 7 }), "src: required");
    refuses(json!({ "src": SRC, "title": 7 }), "title: must be a string");
    refuses(
        json!({ "src": SRC, "title": null }),
        "title: must be a string",
    );
    refuses(
        json!({ "src": SRC, "height": "720" }),
        "height: must be a number",
    );
    refuses(
        json!({ "src": SRC, "height": 80 }),
        "height: must be a number",
    );
    refuses(
        json!({ "src": SRC, "height": 9000 }),
        "height: must be a number",
    );
    let long = format!("{SRC}/{}", "a".repeat(2048));
    refuses(json!({ "src": long }), "src: string too long");
}
