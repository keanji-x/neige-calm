//! Shared plugin protocol vocabulary, independent of transport and persistence.

/// Reserved namespace for overlay rows authored by the kernel itself.
pub const KERNEL_OVERLAY_PLUGIN_ID: &str = "kernel";

/// MCP annotations for a declared read-only tool.
pub fn read_only_annotations() -> serde_json::Value {
    serde_json::json!({ "readOnlyHint": true })
}
