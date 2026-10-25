//! Shared classification of public proxy configuration.
use std::ffi::OsStr;

pub const PROXY_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// URL proxy values require a parsed host without userinfo and no raw `@`.
/// Exclusion lists need no URL parser: host patterns never require `@`.
/// Parsing can normalize authority delimiters and empty userinfo away. Conservatively
/// treat every raw `@` as secret, even in a path, query or fragment, rather than
/// duplicate the parser's authority splitting and risk persisting credentials.
pub fn proxy_value_is_secret(key: &str, value: &OsStr) -> bool {
    let Some(value) = value.to_str() else {
        return true;
    };
    if matches!(key, "NO_PROXY" | "no_proxy") {
        return value.contains('@');
    }
    let Ok(url) = url::Url::parse(value) else {
        return true;
    };
    !url.has_host() || !url.username().is_empty() || url.password().is_some() || value.contains('@')
}
