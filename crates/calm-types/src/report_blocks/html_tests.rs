//! `html` payloads through the one write-end validator, [`validate_payload`], and the one `path`
//! rule pinned by the fixture the frontend reads too (`test-data/report-html-path-v1.json`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::report_blocks::{KIND_HTML, MAX_STRING_CHARS, validate_payload};

const PATH: &str = "docs/overview.html";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    accept: Vec<String>,
    refuse: Vec<String>,
}

fn cases() -> Cases {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/report-html-path-v1.json"
    )))
    .expect("report-html-path-v1.json parses")
}

fn accepts(path: &str) -> bool {
    validate_payload(KIND_HTML, &json!({ "path": path })).is_ok()
}

#[test]
fn the_validator_agrees_with_every_fixture_case() {
    let cases = cases();
    assert!(
        cases.accept.len() >= 5 && cases.refuse.len() >= 20,
        "anti-vacuity"
    );
    let wrong: Vec<(&String, bool)> = cases
        .accept
        .iter()
        .map(|path| (path, true))
        .chain(cases.refuse.iter().map(|path| (path, false)))
        .filter(|(path, want)| accepts(path) != *want)
        .collect();
    assert!(
        wrong.is_empty(),
        "validator disagrees (path, want): {wrong:?}"
    );
}

fn refuses(payload: Value, needle: &str) {
    let err = validate_payload(KIND_HTML, &payload).expect_err(&payload.to_string());
    assert!(err.contains(needle), "{payload} → {err}");
}

#[test]
fn html_accepts_optional_title_and_height() {
    assert_eq!(
        validate_payload(
            KIND_HTML,
            &json!({ "path": PATH, "title": "Architecture", "height": 640 })
        ),
        Ok(())
    );
    for height in [120, 2000] {
        assert_eq!(
            validate_payload(KIND_HTML, &json!({ "path": PATH, "height": height })),
            Ok(())
        );
    }
}

#[test]
fn html_refuses_each_path_hazard_by_name() {
    for path in [
        "/docs/overview.html",
        "https://calm.example/overview.html",
        "docs/../overview.html",
        "../overview.html",
        "docs\\overview.html",
        "docs/overview.html?x=1",
        "docs/overview.html#top",
        "docs/over\u{0}view.html",
        "docs/over\u{85}view.html",
        "docs/overview.txt",
    ] {
        refuses(
            json!({ "path": path }),
            "path: required workspace-relative path",
        );
    }
}

#[test]
fn html_refuses_unknown_fields_and_mistyped_title_or_height() {
    refuses(json!({ "path": PATH, "src": PATH }), "src: unknown field");
    refuses(json!({}), "path: required");
    refuses(json!({ "path": 7 }), "path: required");
    refuses(
        json!({ "path": PATH, "title": 7 }),
        "title: must be a string",
    );
    refuses(
        json!({ "path": PATH, "title": null }),
        "title: must be a string",
    );
    refuses(
        json!({ "path": PATH, "height": "480" }),
        "height: must be a number",
    );
    refuses(
        json!({ "path": PATH, "height": 119 }),
        "height: must be a number",
    );
    refuses(
        json!({ "path": PATH, "height": 2001 }),
        "height: must be a number",
    );
    let long = format!("{}/x.html", "a".repeat(MAX_STRING_CHARS));
    refuses(json!({ "path": long }), "path: string too long");
}
