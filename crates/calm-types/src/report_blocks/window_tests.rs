//! `window` payloads through the one write-end validator, [`validate_payload`].

use serde_json::{Value, json};

use crate::report_blocks::{KIND_WINDOW, validate_payload};

const SRC: &str = "/api/plugins/desktop/ws/apps/chrome/stream";

fn refuses(payload: Value, needle: &str) {
    let err = validate_payload(KIND_WINDOW, &payload).expect_err(&payload.to_string());
    assert!(err.contains(needle), "{payload} → {err}");
}

fn refuses_src(src: &str) {
    refuses(json!({ "src": src }), "src: required");
}

#[test]
fn window_accepts_a_plugin_socket_path_with_optional_title_and_height() {
    assert_eq!(
        validate_payload(
            KIND_WINDOW,
            &json!({ "src": SRC, "title": "Chrome", "height": 720 })
        ),
        Ok(())
    );
    for src in [
        SRC,
        // A grandfathered id with `-` and an id with `.` are both loadable plugin ids.
        "/api/plugins/dev-neige-market/ws/s",
        "/api/plugins/a.b/ws/s",
        // Dots and `%2e` inside a longer segment are not dot segments.
        "/api/plugins/desktop/ws/.hidden/a..b/...",
        "/api/plugins/desktop/ws/a%2eb/%2e%2e%2e",
        "/api/plugins/desktop/ws/%41pp",
    ] {
        assert_eq!(
            validate_payload(KIND_WINDOW, &json!({ "src": src })),
            Ok(()),
            "{src}"
        );
    }
}

#[test]
fn window_src_must_sit_under_the_plugin_socket_route() {
    for src in [
        "/apps/screener",
        "/api/terminals/1",
        "/x/api/plugins/desktop/ws/s",
        "/API/plugins/desktop/ws/s",
        "/api/plugin/desktop/ws/s",
        "/api/plugins/desktop/http/s",
        "/api/plugins/desktop/WS/s",
        "/api/plugins/desktop/ws",
        "/api/plugins/desktop/ws/",
        "/api/plugins//ws/s",
        "/api/plugins/d/ws/s",
        "/api/plugins/Desktop/ws/s",
        "/api/plugins/de_sk/ws/s",
        "/api/plugins/-desk/ws/s",
        "/api/plugins/desk%74op/ws/s",
    ] {
        refuses_src(src);
    }
}

#[test]
fn window_src_refuses_absolute_and_protocol_relative_urls() {
    for src in [
        "https://calm.example/api/plugins/desktop/ws/s",
        "wss://calm.example/api/plugins/desktop/ws/s",
        "//calm.example/api/plugins/desktop/ws/s",
        "api/plugins/desktop/ws/s",
        "/\\calm.example/api/plugins/desktop/ws/s",
        "/api/plugins/desktop/ws/a\\b",
        "/api/plugins/desktop/ws/\n/s",
        "/api/plugins/desktop/ws/s\u{7f}",
    ] {
        refuses_src(src);
    }
}

#[test]
fn window_src_refuses_raw_dot_segments() {
    for src in [
        "/api/plugins/desktop/ws/../../terminals/1",
        "/api/plugins/desktop/ws/./s",
        "/api/plugins/desktop/ws/a/../s",
        "/api/plugins/desktop/ws/s/.",
        "/api/plugins/desktop/ws/s/..",
        "/api/plugins/desktop/ws/.",
        "/api/plugins/desktop/ws/..",
    ] {
        refuses_src(src);
    }
}

#[test]
fn window_src_refuses_percent_encoded_dot_segments() {
    for src in [
        "/api/plugins/desktop/ws/%2e/s",
        "/api/plugins/desktop/ws/%2E/s",
        "/api/plugins/desktop/ws/%2e%2e/s",
        "/api/plugins/desktop/ws/%2E%2E/s",
        "/api/plugins/desktop/ws/%2e%2E/s",
        "/api/plugins/desktop/ws/.%2e/s",
        "/api/plugins/desktop/ws/%2E./s",
        "/api/plugins/desktop/ws/s/%2e%2e",
        "/api/plugins/desktop/ws/s/%2E",
    ] {
        refuses_src(src);
    }
}

#[test]
fn window_src_refuses_a_query_or_a_fragment() {
    for src in [
        "/api/plugins/desktop/ws/s?x=1",
        "/api/plugins/desktop/ws/s?",
        "/api/plugins/desktop/ws/s#top",
        "/api/plugins/desktop/ws/s?/../x",
    ] {
        refuses_src(src);
    }
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
