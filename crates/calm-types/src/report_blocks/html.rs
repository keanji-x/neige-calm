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

const HTML_PATH_RULE: &str = "workspace-relative path to a `.html`/`.htm` file with a name \
     before the extension (no leading `/`, no scheme, no `..` segment, no surrounding whitespace, \
     no `\\`, `?`, `#` or control characters)";

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

/// The `html` block's `path` rule. Lengths are checked by the caller's string cap. The Track
/// workspace read trims its path before opening it, so a path with surrounding whitespace would
/// name a different file than the block shows: it is refused, and what is validated is exactly
/// what the route opens.
pub fn is_report_html_path(path: &str) -> bool {
    !path.is_empty()
        && path.trim() == path
        && !path.starts_with('/')
        && !has_scheme(path)
        && !path
            .chars()
            .any(|c| matches!(c, '\\' | '?' | '#') || is_url_hostile_char(c))
        && !path.split('/').any(|segment| segment == "..")
        && is_html_page_name(path)
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

/// The one page-name rule, shared by this validator and the raw route that serves pages: the last
/// segment is `{stem}.html` or `{stem}.htm`, case-insensitive, with a non-empty stem. A bare
/// `.html` is a dotfile, not a page.
pub fn is_html_page_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".html", ".htm"]
        .iter()
        .any(|ext| name.len() > ext.len() && name.ends_with(ext))
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
