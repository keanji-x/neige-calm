//! `window` (#2530): a live window that speaks window-stream protocol v1 at `src`. The kind names
//! no plugin: `src` is a same-origin path under the plugin WebSocket route, and whichever plugin
//! serves that route serves the stream.

use serde_json::{Map, Value};

use super::kinds::{
    SAME_ORIGIN_PATH_RULE, check_string_cap, is_same_origin_path, optional_height, optional_string,
    reject_unknown,
};
use crate::plugin::is_valid_plugin_id;

/// The route a `window` block's `src` must sit under: `/api/plugins/{id}/ws/{path}`.
pub const WINDOW_SRC_PREFIX: &str = "/api/plugins/";

/// [`is_window_src`] as a JSON Schema `pattern` (ECMA-262), for the published `window` schema.
pub const WINDOW_SRC_PATTERN: &str =
    "^(?!.*/(?:\\.|%2[eE]){1,2}(?:/|$))/api/plugins/[a-z0-9][a-z0-9.-]{1,63}/ws/[^?#\\\\]+$";

const WINDOW_SRC_RULE: &str = "`/api/plugins/{plugin id}/ws/{path}` with a non-empty path, no \
     dot segment (`.`, `..` or any `%2e` form: the browser resolves them away), no query and no \
     fragment (a WebSocket URL takes no fragment, and protocol v1 takes no parameters)";

pub(super) fn validate_window(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, &["src", "title", "height"], errors);
    match map.get("src") {
        Some(Value::String(src)) if is_window_src(src) => check_string_cap("src", src, errors),
        _ => errors.push(format!(
            "src: required {SAME_ORIGIN_PATH_RULE}, of the form {WINDOW_SRC_RULE}"
        )),
    }
    optional_string(map, "title", errors);
    optional_height(map, errors);
}

/// A same-origin path (the `app` block's rule) under [`WINDOW_SRC_PREFIX`], naming a valid plugin
/// id, then `/ws/` and a non-empty rest, with no `?`, no `#` and no dot segment in any spelling.
fn is_window_src(src: &str) -> bool {
    let Some((id, path)) = src
        .strip_prefix(WINDOW_SRC_PREFIX)
        .and_then(|rest| rest.split_once("/ws/"))
    else {
        return false;
    };
    is_same_origin_path(src)
        && is_valid_plugin_id(id)
        && !path.is_empty()
        && !src.contains(['?', '#'])
        && !src.split('/').any(is_dot_segment)
}

/// `.` or `..`, with any dot spelled `%2e` or `%2E`: the WHATWG URL parser removes all of them.
fn is_dot_segment(segment: &str) -> bool {
    let decoded = segment.to_ascii_lowercase().replace("%2e", ".");
    decoded == "." || decoded == ".."
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
