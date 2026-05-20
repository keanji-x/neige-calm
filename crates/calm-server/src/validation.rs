//! Per-kind payload validators (D4).
//!
//! The kernel persists two opaque-by-default JSON columns: `Card.payload` and
//! `Overlay.payload`. The architectural invariant is that plugin-defined kinds
//! (anything that isn't built into the kernel vocabulary) stay opaque — the
//! kernel does **not** validate or interpret them.
//!
//! This module narrows that opacity for the small set of kinds the kernel
//! itself owns. For those, we check the JSON shape at every write boundary
//! and reject malformed payloads with `CalmError::BadRequest` (→ HTTP 400).
//! Bad writes used to silently land in the DB; frontend zod schemas only
//! catch them on read.
//!
//! Kinds covered:
//!
//! | Field | Kind | Shape |
//! |---|---|---|
//! | `Card.payload`    | `"terminal"`  | `{ terminal_id?: String }` (optional — freshly-created cards may not yet have one) |
//! | `Overlay.payload` | `"status"`    | `{ state: String }` |
//! | `Overlay.payload` | `"progress"`  | `{ value: f64 }` |
//! | `Overlay.payload` | `"eta"`       | `{ text: String }` |
//! | `Overlay.payload` | `"now"`       | `{ text: String }` |
//!
//! Anything else (`ui://*` cards, plugin-defined overlay kinds) is accepted
//! unchanged — the validator returns `Ok(())` without inspecting the payload.
//!
//! `Plugin.user_config` and `ToolCallBody.arguments` are intentionally NOT
//! covered: those carry per-plugin / per-tool semantics that the kernel has
//! no schema for.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{CalmError, Result};

/// Validate a `Card.payload` for a given `kind`.
///
/// Returns `Ok(())` for every kind the kernel does not own (including all
/// `ui://*` plugin-defined kinds). Returns `Err(CalmError::BadRequest)` when
/// the payload doesn't match the kernel's expected shape.
pub fn validate_card_payload(kind: &str, payload: &Value) -> Result<()> {
    match kind {
        "terminal" => {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct TerminalPayload {
                #[serde(default)]
                terminal_id: Option<String>,
            }
            // Empty object / null payloads are accepted (terminal_id defaults
            // to None — fresh terminal cards may not yet be bound to a PTY).
            if payload.is_null() {
                return Ok(());
            }
            serde_json::from_value::<TerminalPayload>(payload.clone())
                .map(|_| ())
                .map_err(|e| CalmError::BadRequest(format!("invalid terminal payload: {e}")))
        }
        // Plugin-defined kinds are opaque per architectural invariant.
        _ => Ok(()),
    }
}

/// Validate an `Overlay.payload` for a given `kind`.
///
/// Returns `Ok(())` for unknown / plugin-specific kinds. Returns
/// `Err(CalmError::BadRequest)` when a kernel-owned kind has the wrong shape.
pub fn validate_overlay_payload(kind: &str, payload: &Value) -> Result<()> {
    match kind {
        "status" => {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct StatusPayload {
                state: String,
            }
            serde_json::from_value::<StatusPayload>(payload.clone())
                .map(|_| ())
                .map_err(|e| CalmError::BadRequest(format!("invalid status payload: {e}")))
        }
        "progress" => {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct ProgressPayload {
                value: f64,
            }
            serde_json::from_value::<ProgressPayload>(payload.clone())
                .map(|_| ())
                .map_err(|e| CalmError::BadRequest(format!("invalid progress payload: {e}")))
        }
        "eta" => {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct EtaPayload {
                text: String,
            }
            serde_json::from_value::<EtaPayload>(payload.clone())
                .map(|_| ())
                .map_err(|e| CalmError::BadRequest(format!("invalid eta payload: {e}")))
        }
        "now" => {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct NowPayload {
                text: String,
            }
            serde_json::from_value::<NowPayload>(payload.clone())
                .map(|_| ())
                .map_err(|e| CalmError::BadRequest(format!("invalid now payload: {e}")))
        }
        // Plugin-defined overlay kinds stay opaque.
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------------- Card: terminal ----------------

    #[test]
    fn terminal_happy_with_id() {
        validate_card_payload("terminal", &json!({ "terminal_id": "t1" })).unwrap();
    }

    #[test]
    fn terminal_happy_without_id() {
        validate_card_payload("terminal", &json!({})).unwrap();
    }

    #[test]
    fn terminal_happy_null() {
        validate_card_payload("terminal", &Value::Null).unwrap();
    }

    #[test]
    fn terminal_extra_fields_tolerated() {
        // Unknown fields stay in the JSON — serde ignores them by default.
        validate_card_payload("terminal", &json!({ "terminal_id": "t1", "extra": "ok" })).unwrap();
    }

    #[test]
    fn terminal_rejects_wrong_type() {
        let err = validate_card_payload("terminal", &json!({ "terminal_id": 42 })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn terminal_rejects_array_root() {
        let err = validate_card_payload("terminal", &json!([1, 2, 3])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Card: opt-out for plugin kinds ----------------

    #[test]
    fn ui_prefixed_card_accepts_anything() {
        // Acceptance criterion: a junk payload under a ui://* kind must NOT 400.
        validate_card_payload("ui://example/view", &json!({ "junk": "ok" })).unwrap();
        validate_card_payload("ui://example/view", &json!([1, 2, 3])).unwrap();
        validate_card_payload("ui://example/view", &Value::Null).unwrap();
    }

    #[test]
    fn plugin_prefixed_card_accepts_anything() {
        validate_card_payload("plugin:foo:bar", &json!({ "whatever": true })).unwrap();
    }

    // ---------------- Overlay: status ----------------

    #[test]
    fn status_happy() {
        validate_overlay_payload("status", &json!({ "state": "running" })).unwrap();
    }

    #[test]
    fn status_rejects_missing_state() {
        let err = validate_overlay_payload("status", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn status_rejects_wrong_type() {
        let err = validate_overlay_payload("status", &json!({ "state": 42 })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Overlay: progress ----------------

    #[test]
    fn progress_happy() {
        validate_overlay_payload("progress", &json!({ "value": 0.42 })).unwrap();
    }

    #[test]
    fn progress_happy_integer() {
        // serde_json accepts integers as f64.
        validate_overlay_payload("progress", &json!({ "value": 1 })).unwrap();
    }

    #[test]
    fn progress_rejects_missing_value() {
        let err = validate_overlay_payload("progress", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn progress_rejects_string_value() {
        let err = validate_overlay_payload("progress", &json!({ "value": "fast" })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Overlay: eta ----------------

    #[test]
    fn eta_happy() {
        validate_overlay_payload("eta", &json!({ "text": "5m" })).unwrap();
    }

    #[test]
    fn eta_rejects_missing_text() {
        let err = validate_overlay_payload("eta", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn eta_rejects_wrong_type() {
        let err = validate_overlay_payload("eta", &json!({ "text": 5 })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Overlay: now ----------------

    #[test]
    fn now_happy() {
        validate_overlay_payload("now", &json!({ "text": "writing tests" })).unwrap();
    }

    #[test]
    fn now_rejects_missing_text() {
        let err = validate_overlay_payload("now", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn now_rejects_wrong_type() {
        let err = validate_overlay_payload("now", &json!({ "text": null })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Overlay: unknown / opaque kinds ----------------

    #[test]
    fn unknown_overlay_kind_accepts_anything() {
        validate_overlay_payload("custom-plugin-kind", &json!({ "anything": true })).unwrap();
        validate_overlay_payload("custom-plugin-kind", &json!([])).unwrap();
        validate_overlay_payload("custom-plugin-kind", &Value::Null).unwrap();
    }
}
