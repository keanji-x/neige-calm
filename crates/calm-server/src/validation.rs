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
//! | `Overlay.payload` | `"layout"`    | `{ positions: { <card_id>: { x,y,w,h: u32 }, … } }` |
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
        "codex" => {
            // Codex cards carry an opaque blob with the original spawn
            // params (initial_prompt, model, cwd) for diagnostics/replay.
            // We don't pin a strict shape — the route reads the body
            // separately, the payload is purely for the UI.
            if payload.is_null() || payload.is_object() {
                Ok(())
            } else {
                Err(CalmError::BadRequest(
                    "codex payload must be an object or null".into(),
                ))
            }
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
        "layout" => validate_layout_payload(payload),
        // Plugin-defined overlay kinds stay opaque.
        _ => Ok(()),
    }
}

/// Grid column count — mirrors `web/src/WaveGrid.tsx::COLS`. Any layout
/// whose `x + w` exceeds this would render off-screen, so the kernel
/// rejects it at the write boundary rather than coping with the resulting
/// half-broken RGL state on the client. If `COLS` ever changes on the
/// frontend, this constant must move in lock-step.
const LAYOUT_GRID_COLS: u32 = 12;

/// Validate a `layout` overlay payload — the WaveGrid card position
/// record that backs `useOverlayState({ entity_kind: 'view', kind: 'layout' })`
/// per design doc §5.2.
///
/// Schema (strict — unknown fields anywhere reject):
/// ```text
/// {
///   "positions": {
///     "<card_id>": { "x": <u32>, "y": <u32>, "w": <u32>, "h": <u32> },
///     ...
///   }
/// }
/// ```
///
/// Geometry constraints:
///   * `w >= 1`, `h >= 1`
///   * `x + w <= LAYOUT_GRID_COLS` (`= 12`)
///   * card_id keys must be non-empty
fn validate_layout_payload(payload: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LayoutPayload {
        positions: std::collections::BTreeMap<String, LayoutPos>,
    }

    // `y` is parsed (to enforce the `u32` non-negativity bound + the
    // `deny_unknown_fields` strictness) but isn't otherwise checked — RGL
    // doesn't have a "max rows" concept; cards just keep stacking down.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LayoutPos {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    }

    let parsed: LayoutPayload = serde_json::from_value(payload.clone())
        .map_err(|e| CalmError::BadRequest(format!("invalid layout payload: {e}")))?;

    for (card_id, pos) in &parsed.positions {
        if card_id.is_empty() {
            return Err(CalmError::BadRequest(
                "invalid layout payload: positions key must be a non-empty card id".into(),
            ));
        }
        if pos.w < 1 {
            return Err(CalmError::BadRequest(format!(
                "invalid layout payload: positions.{card_id}.w must be >= 1, got {}",
                pos.w
            )));
        }
        if pos.h < 1 {
            return Err(CalmError::BadRequest(format!(
                "invalid layout payload: positions.{card_id}.h must be >= 1, got {}",
                pos.h
            )));
        }
        // `u32` already excludes negatives; only the grid-column bound needs
        // a check. Use `checked_add` so an attacker can't smuggle through an
        // overflowed sum that wraps under `LAYOUT_GRID_COLS`.
        match pos.x.checked_add(pos.w) {
            Some(sum) if sum <= LAYOUT_GRID_COLS => {}
            _ => {
                return Err(CalmError::BadRequest(format!(
                    "invalid layout payload: positions.{card_id}.x + w must be <= {} (grid columns), got x={} w={}",
                    LAYOUT_GRID_COLS, pos.x, pos.w
                )));
            }
        }
    }

    Ok(())
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

    // ---------------- Overlay: layout ----------------

    #[test]
    fn layout_happy_empty_positions() {
        validate_overlay_payload("layout", &json!({ "positions": {} })).unwrap();
    }

    #[test]
    fn layout_happy_one_card() {
        validate_overlay_payload(
            "layout",
            &json!({ "positions": { "card-1": { "x": 0, "y": 0, "w": 4, "h": 3 } } }),
        )
        .unwrap();
    }

    #[test]
    fn layout_happy_card_at_right_edge() {
        // `x + w == COLS` is allowed (exact fit, no overflow).
        validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 8, "y": 0, "w": 4, "h": 2 } } }),
        )
        .unwrap();
    }

    #[test]
    fn layout_rejects_missing_positions() {
        let err = validate_overlay_payload("layout", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_positions_not_object() {
        let err = validate_overlay_payload("layout", &json!({ "positions": [] })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_x_plus_w_over_cols() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 10, "y": 0, "w": 4, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("grid columns")));
    }

    #[test]
    fn layout_rejects_w_zero() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 0, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("w must be >= 1")));
    }

    #[test]
    fn layout_rejects_h_zero() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2, "h": 0 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("h must be >= 1")));
    }

    #[test]
    fn layout_rejects_negative_x() {
        // serde_json refuses to coerce a negative number into `u32` —
        // the deserialize step returns BadRequest.
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": -1, "y": 0, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_negative_y() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": -1, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_missing_position_field() {
        // Missing `h` — serde rejects.
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_unknown_root_field() {
        let err = validate_overlay_payload("layout", &json!({ "positions": {}, "extra": 1 }))
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_unknown_position_field() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2, "h": 2, "z": 9 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn layout_rejects_empty_card_id_key() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "": { "x": 0, "y": 0, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("non-empty card id")));
    }
}
