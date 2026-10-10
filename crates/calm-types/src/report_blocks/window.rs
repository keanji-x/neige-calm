//! `window` (#2530): a live window that speaks window-stream protocol v1 at `src`. The kind names
//! no plugin: `src` is a path under the plugin WebSocket route, and whichever plugin serves that
//! route serves the stream.
//!
//! `src` is an allowlist, not a blocklist: every byte is one the browser's URL parser keeps as is,
//! so no spelling of a dot segment, separator, space or control character can make the browser
//! open another path than the one validated here.

use serde_json::{Map, Value};

use super::kinds::{check_string_cap, optional_height, optional_string, reject_unknown};
use crate::plugin::is_valid_plugin_id;

/// The route a `window` block's `src` must sit under: `/api/plugins/{id}/ws/{path}`.
pub const WINDOW_SRC_PREFIX: &str = "/api/plugins/";

/// The one `src` rule, published verbatim in the report contract and used verbatim by the
/// frontend. [`is_window_src`] is its hand matcher; `test-data/window-src-v1.json` pins both, and
/// the plugin id part to [`is_valid_plugin_id`].
pub const WINDOW_SRC_PATTERN: &str =
    "^/api/plugins/[a-z0-9][a-z0-9.-]{1,63}/ws/[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*$";

const WINDOW_SRC_RULE: &str = "`/api/plugins/{plugin id}/ws/{segment}[/{segment}…]`, each \
     segment one or more of `A-Z a-z 0-9 _ -` (no dots, `%`, spaces, query or fragment)";

pub(super) fn validate_window(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, &["src", "title", "height"], errors);
    match map.get("src") {
        Some(Value::String(src)) if is_window_src(src) => check_string_cap("src", src, errors),
        _ => errors.push(format!("src: required path {WINDOW_SRC_RULE}")),
    }
    optional_string(map, "title", errors);
    optional_height(map, errors);
}

/// [`WINDOW_SRC_PATTERN`], matched by hand so calm-types needs no regex engine.
fn is_window_src(src: &str) -> bool {
    let Some((id, path)) = src
        .strip_prefix(WINDOW_SRC_PREFIX)
        .and_then(|rest| rest.split_once("/ws/"))
    else {
        return false;
    };
    is_valid_plugin_id(id) && path.split('/').all(is_path_segment)
}

/// `[A-Za-z0-9_-]+`.
fn is_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
