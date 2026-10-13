//! `html` (#2585): a self-contained HTML page in the Track workspace, shown inline in a sandboxed
//! frame. The block names the page by `path`; the kernel serves it from the Track's own raw route
//! under a script-only sandbox CSP, so the page can draw and run its own scripts but cannot reach
//! the app, the API or the network.
//!
//! `path` is the one rule here, pinned by `test-data/report-html-path-v1.json`, which the frontend
//! reads too. It matches what the Track workspace read accepts (relative, no `..` segment) and
//! adds what a page link must not carry: a scheme, `\`, `?`, `#` or a control character.

use serde_json::{Map, Value};

use super::kinds::{
    check_string_cap, is_url_hostile_char, optional_height, optional_string, reject_unknown,
};

const HTML_PATH_RULE: &str = "workspace-relative path to a `.html`/`.htm` file (no leading `/`, \
     no scheme, no `..` segment, no `\\`, `?`, `#` or control characters)";

pub(super) fn validate_html(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, &["path", "title", "height"], errors);
    match map.get("path") {
        Some(Value::String(path)) if is_report_html_path(path) => {
            check_string_cap("path", path, errors);
        }
        _ => errors.push(format!("path: required {HTML_PATH_RULE}")),
    }
    optional_string(map, "title", errors);
    optional_height(map, errors);
}

/// The `html` block's `path` rule. Lengths are checked by the caller's string cap.
pub fn is_report_html_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !has_scheme(path)
        && !path
            .chars()
            .any(|c| matches!(c, '\\' | '?' | '#') || is_url_hostile_char(c))
        && !path.split('/').any(|segment| segment == "..")
        && has_html_extension(path)
}

/// `^[A-Za-z][A-Za-z0-9+.-]*:` — anything a URL parser would read as a scheme.
fn has_scheme(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn has_html_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
