//! Shared plugin protocol vocabulary, independent of transport and persistence.

/// Reserved namespace for overlay rows authored by the kernel itself.
pub const KERNEL_OVERLAY_PLUGIN_ID: &str = "kernel";

/// The loadable plugin id rule, `^[a-z0-9][a-z0-9.-]{1,63}$`: 2..=64 chars with an alphanumeric
/// head. The manifest loader and every contract that names a plugin in a path use this one rule.
pub fn is_valid_plugin_id(s: &str) -> bool {
    let lower_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let bytes = s.as_bytes();
    (2..=64).contains(&bytes.len())
        && lower_alnum(bytes[0])
        && bytes[1..]
            .iter()
            .all(|&b| lower_alnum(b) || b == b'.' || b == b'-')
}

/// MCP annotations for a declared read-only tool.
pub fn read_only_annotations() -> serde_json::Value {
    serde_json::json!({ "readOnlyHint": true })
}

/// Write tools whose declared roles are enforced by the kernel registry.
pub fn role_gated_write_annotations() -> serde_json::Value {
    serde_json::json!({"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false})
}
