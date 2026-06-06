//! Per-kind payload validators (D4) and per-kind schema versions.
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
//! | `Card.payload`    | `"codex"`     | object or null (opaque diagnostic blob) |
//! | `Overlay.payload` | `"status"`    | `{ state: String }` |
//! | `Overlay.payload` | `"progress"`  | `{ value: f64 }` |
//! | `Overlay.payload` | `"eta"`       | `{ text: String }` |
//! | `Overlay.payload` | `"now"`       | `{ text: String }` |
//! | `Overlay.payload` | `"layout"`    | `{ positions: { <card_id>: { x,y,w,h: u32 }, … } }` |
//! | `Overlay.payload` | `"any_card_needs_input"` | `{ value: bool }` (wave-scoped — see issue #254) |
//!
//! Anything else (`ui://*` cards, plugin-defined overlay kinds) is accepted
//! unchanged — the validator returns `Ok(())` without inspecting the payload.
//!
//! `Plugin.user_config` and `ToolCallBody.arguments` are intentionally NOT
//! covered: those carry per-plugin / per-tool semantics that the kernel has
//! no schema for.
//!
//! ## `schemaVersion` (Tier A — upgrade-stability policy)
//!
//! Per `docs/upgrade-stability.md`, kernel-owned card and overlay payloads
//! are a Tier A persistence contract. Each kernel-owned kind carries a
//! `schemaVersion: u32` constant; at write time the validator enforces:
//!
//!   * absent `schemaVersion` → accepted, treated as version 1 (the only
//!     version that has ever existed for any of these kinds today, so
//!     historical rows written before this field was introduced are
//!     backward-compatible without a DB migration);
//!   * present and matching the per-kind constant → accepted;
//!   * present and any other value → rejected with `CalmError::BadRequest`
//!     carrying a "kernel supports N, got M" message so old binaries refuse
//!     to silently process payloads from future ones.
//!
//! Plugin-owned overlay payloads are explicitly **not** inspected for a
//! `schemaVersion` — they pass through opaquely (no version policy from us).

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::model::Overlay;

// ---------------- Per-kind schema versions (Tier A) ----------------
//
// One constant per kernel-owned kind. Bumping these is a Tier A breaking
// change: the same PR that bumps a version must add the migrator helper
// for older rows in `payload_schema_version`'s neighborhood (see the
// comment there). All start at `1` — the only shape any of these kinds
// has ever had.

/// `schemaVersion` for `Card.payload` when `kind == "terminal"`.
pub const TERMINAL_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Card.payload` when `kind == "codex"`.
pub const CODEX_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Card.payload` when `kind == "claude"`.
pub const CLAUDE_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Card.payload` when `kind == "wave-report"` (issue
/// #229 PR B). Mirrors [`crate::wave_report::WaveReportPayload::SCHEMA_VERSION`].
pub const WAVE_REPORT_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "status"`.
pub const OVERLAY_STATUS_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "progress"`.
pub const OVERLAY_PROGRESS_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "eta"`.
pub const OVERLAY_ETA_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "now"`.
pub const OVERLAY_NOW_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "layout"`.
pub const OVERLAY_LAYOUT_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "file-viewer-nav"`.
pub const OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "any_card_needs_input"`
/// — the wave-scoped boolean aggregate written by `card_fsm` (issue #254).
pub const OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy)]
pub struct OverlayKindEntry {
    pub kind: &'static str,
    pub validate: fn(&Value) -> Result<()>,
    pub max_schema_version: u32,
}

pub struct OverlayKindRegistry {
    entries: &'static [OverlayKindEntry],
}

impl OverlayKindRegistry {
    pub const fn new(entries: &'static [OverlayKindEntry]) -> Self {
        Self { entries }
    }

    pub fn lookup(&self, kind: &str) -> Option<&'static OverlayKindEntry> {
        self.entries.iter().find(|entry| entry.kind == kind)
    }

    pub fn validate(&self, kind: &str, payload: &Value) -> Result<()> {
        let Some(entry) = self.lookup(kind) else {
            return Ok(());
        };
        (entry.validate)(payload)
    }

    pub fn max_supported_schema_version(&self, kind: &str) -> Option<u32> {
        self.lookup(kind).map(|entry| entry.max_schema_version)
    }
}

fn validate_as<T>(kind: &str, label: &str, max_version: u32, payload: &Value) -> Result<()>
where
    T: DeserializeOwned,
{
    check_schema_version(kind, payload, max_version)?;
    serde_json::from_value::<T>(payload.clone())
        .map(|_| ())
        .map_err(|e| CalmError::BadRequest(format!("invalid {label} payload: {e}")))
}

macro_rules! simple_overlay {
    ($fn_name:ident, $shape_name:ident, $kind:literal, $label:literal, $version:expr, {
        $field:ident: $field_ty:ty
    }) => {
        fn $fn_name(payload: &Value) -> Result<()> {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct $shape_name {
                $field: $field_ty,
            }
            validate_as::<$shape_name>($kind, $label, $version, payload)
        }
    };
}

simple_overlay!(
    validate_status_overlay_payload,
    StatusPayload,
    "status",
    "status",
    OVERLAY_STATUS_SCHEMA_VERSION,
    { state: String }
);
simple_overlay!(
    validate_progress_overlay_payload,
    ProgressPayload,
    "progress",
    "progress",
    OVERLAY_PROGRESS_SCHEMA_VERSION,
    { value: f64 }
);
simple_overlay!(
    validate_eta_overlay_payload,
    EtaPayload,
    "eta",
    "eta",
    OVERLAY_ETA_SCHEMA_VERSION,
    { text: String }
);
simple_overlay!(
    validate_now_overlay_payload,
    NowPayload,
    "now",
    "now",
    OVERLAY_NOW_SCHEMA_VERSION,
    { text: String }
);

fn validate_layout_overlay_payload(payload: &Value) -> Result<()> {
    check_schema_version("layout", payload, OVERLAY_LAYOUT_SCHEMA_VERSION)?;
    validate_layout_payload(payload)
}

fn validate_file_viewer_nav_overlay_payload(payload: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum FileViewerTab {
        Code,
        Diff,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FileViewerNavPayload {
        #[serde(default)]
        schema_version: Option<u32>,
        tab: FileViewerTab,
        folder_path: String,
        selected_path: Option<String>,
        diff_selected: Option<String>,
    }

    validate_as::<FileViewerNavPayload>(
        "file-viewer-nav",
        "file-viewer-nav",
        OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION,
        payload,
    )
}

fn validate_any_card_needs_input_overlay_payload(payload: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct AnyCardNeedsInputPayload {
        #[serde(default)]
        #[serde(rename = "schemaVersion")]
        schema_version: Option<u32>,
        value: bool,
    }

    validate_as::<AnyCardNeedsInputPayload>(
        "any_card_needs_input",
        "any_card_needs_input",
        OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION,
        payload,
    )
}

pub static OVERLAY_KIND_REGISTRY: OverlayKindRegistry = OverlayKindRegistry::new(&[
    OverlayKindEntry {
        kind: "status",
        validate: validate_status_overlay_payload,
        max_schema_version: OVERLAY_STATUS_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "progress",
        validate: validate_progress_overlay_payload,
        max_schema_version: OVERLAY_PROGRESS_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "eta",
        validate: validate_eta_overlay_payload,
        max_schema_version: OVERLAY_ETA_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "now",
        validate: validate_now_overlay_payload,
        max_schema_version: OVERLAY_NOW_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "layout",
        validate: validate_layout_overlay_payload,
        max_schema_version: OVERLAY_LAYOUT_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "file-viewer-nav",
        validate: validate_file_viewer_nav_overlay_payload,
        max_schema_version: OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "any_card_needs_input",
        validate: validate_any_card_needs_input_overlay_payload,
        max_schema_version: OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION,
    },
]);

/// Return the maximum `schemaVersion` this kernel knows how to interpret for
/// an overlay `kind`. `Some(N)` for kernel-owned kinds; `None` for
/// plugin-defined kinds (which we keep fully opaque — no version policy).
///
/// Used by the overlay read-side guard (see `routes::overlays::list_overlays`)
/// to filter out rows that a future kernel wrote at a higher `schemaVersion`
/// than this binary supports. The write path's `check_schema_version` already
/// rejects future versions on ingest; this helper lets the read path do the
/// same for rows that snuck in via a newer binary on the same DB (downgrade
/// or split-deploy scenarios — see issue #198 concern 4).
///
/// Bumping any of the kernel-owned constants above automatically widens what
/// this returns, so the read guard tracks the write guard without a separate
/// update.
pub fn max_supported_overlay_schema_version(kind: &str) -> Option<u32> {
    OVERLAY_KIND_REGISTRY.max_supported_schema_version(kind)
}

/// Read the `schemaVersion` field from a payload, defaulting to `1` when
/// the field is absent or unparsable.
///
/// Treating absent-as-1 means rows written before this field existed
/// keep reading correctly with no DB migration — every kernel-owned kind
/// only has version 1 today, so the missing field is unambiguous.
///
/// `// migrators will live here when v2 is introduced` — once any kind
/// gets a v2, the rule shifts from "absent → 1" to "absent → 1, then
/// run the v1→current migrator on the parsed shape". That migrator lives
/// adjacent to this helper, not behind it; the helper itself stays a
/// trivial reader.
pub fn payload_schema_version(payload: &Value) -> u32 {
    payload
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(1)
}

/// Per-row predicate behind the overlay read-side guard: return `true` if the
/// given overlay row carries a `schemaVersion` higher than this binary's
/// max for its kind, and so must be dropped before being handed to a client
/// (HTTP route response or `/api/events` WS frame).
///
/// Returns `false` (keep the row) for:
///   * plugin-owned kinds (no kernel version policy);
///   * kernel-owned kinds at or below the supported version.
///
/// When the row is filtered out, a structured `tracing::warn!` records the
/// reason — matches the behavior of [`filter_unsupported_overlay_versions`]
/// in `routes::overlays`, which is now a thin wrapper around this helper.
///
/// Lives here in `validation.rs` (rather than in `routes::overlays`) so the
/// WS broadcast/replay path in `ws::events` can call it without a routes →
/// ws dependency. Followup to PR #214 (issue #198 concern 4) — see that PR
/// for the wider rationale on why kernel-owned overlay payloads need a
/// read-side guard at every surface that ships them to a client.
pub fn should_skip_overlay(overlay: &Overlay) -> bool {
    let Some(max) = max_supported_overlay_schema_version(&overlay.kind) else {
        // Plugin-owned kind — opaque, no version policy.
        return false;
    };
    let version = payload_schema_version(&overlay.payload);
    if version > max {
        tracing::warn!(
            overlay_id = %overlay.id,
            kind = %overlay.kind,
            schema_version = version,
            max_supported = max,
            entity_kind = %overlay.entity_kind,
            entity_id = %overlay.entity_id,
            "dropping overlay with unsupported schemaVersion on read \
             (kernel-owned kind, future version); upgrade this binary \
             or rewrite the row to the supported version",
        );
        true
    } else {
        false
    }
}

/// Extension of [`should_skip_overlay`] to the broadcast/replay surface:
/// given an `Event` about to be shipped over `/api/events`, return `true`
/// if the event embeds an overlay row with an unsupported `schemaVersion`
/// (the `Event::OverlaySet` variant) and so must not reach the client.
///
/// Only `Event::OverlaySet(Overlay)` ships a full `Overlay` payload across
/// the WS wire today — `Event::OverlayDeleted` carries only id metadata, so
/// there is no payload to gate. Every other variant returns `false`
/// (forward as usual).
///
/// This helper is the single point of policy for the WS write barrier, so
/// future overlay-bearing event variants only need to extend the match arm
/// here to inherit the guard at both the live-broadcast and replay
/// sites in `ws::events`.
pub fn should_skip_event_for_overlay_version(event: &Event) -> bool {
    match event {
        Event::OverlaySet(overlay) => should_skip_overlay(overlay),
        _ => false,
    }
}

/// Enforce the `schemaVersion` rule for a kernel-owned kind:
///
///   * absent → accept (treated as `expected`);
///   * present and `== expected` → accept;
///   * any other value → `BadRequest`.
fn check_schema_version(kind: &str, payload: &Value, expected: u32) -> Result<()> {
    // Non-object payloads (null, scalar, array) can't carry a
    // `schemaVersion` field by construction — the kind-specific validator
    // owns whether those are accepted; this check stays out of the way.
    if !payload.is_object() {
        return Ok(());
    }
    let Some(raw) = payload.get("schemaVersion") else {
        return Ok(());
    };
    let Some(version) = raw.as_u64() else {
        return Err(CalmError::BadRequest(format!(
            "invalid schemaVersion for kind `{kind}`: expected u32, got {raw}"
        )));
    };
    if version as u32 == expected {
        Ok(())
    } else {
        Err(CalmError::BadRequest(format!(
            "unsupported schemaVersion {version} for kind `{kind}`; this kernel supports {expected}"
        )))
    }
}

/// Validate an `Overlay.payload` for a given `kind`.
///
/// Returns `Ok(())` for unknown / plugin-specific kinds. Returns
/// `Err(CalmError::BadRequest)` when a kernel-owned kind has the wrong shape.
pub fn validate_overlay_payload(kind: &str, payload: &Value) -> Result<()> {
    OVERLAY_KIND_REGISTRY.validate(kind, payload)
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
    // `deny_unknown_fields` stays on so a typo in the writer (e.g. a stray
    // `positoins` key) is caught at the boundary. We allow `schemaVersion`
    // explicitly because every kernel-owned payload now carries it on
    // write; per-kind value enforcement happens in `check_schema_version`
    // before we get here.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LayoutPayload {
        positions: std::collections::BTreeMap<String, LayoutPos>,
        #[serde(default, rename = "schemaVersion")]
        schema_version: Option<u32>,
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
    use crate::card_kind::CardKindRegistry;
    use serde_json::json;

    fn validate_builtin_card(kind: &str, payload: &Value) -> Result<()> {
        CardKindRegistry::builtins()
            .validate_payload(kind, payload)
            .map_err(CalmError::from)
    }

    // ---------------- Card: terminal ----------------

    #[test]
    fn terminal_happy_with_id() {
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1" })).unwrap();
    }

    #[test]
    fn terminal_happy_without_id() {
        validate_builtin_card("terminal", &json!({})).unwrap();
    }

    #[test]
    fn terminal_happy_null() {
        validate_builtin_card("terminal", &Value::Null).unwrap();
    }

    #[test]
    fn terminal_extra_fields_tolerated() {
        // Unknown fields stay in the JSON — serde ignores them by default.
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1", "extra": "ok" })).unwrap();
    }

    #[test]
    fn terminal_rejects_wrong_type() {
        let err = validate_builtin_card("terminal", &json!({ "terminal_id": 42 })).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn terminal_rejects_array_root() {
        let err = validate_builtin_card("terminal", &json!([1, 2, 3])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Card: opt-out for plugin kinds ----------------

    #[test]
    fn ui_prefixed_card_accepts_anything() {
        // Acceptance criterion: a junk payload under a ui://* kind must NOT 400.
        validate_builtin_card("ui://example/view", &json!({ "junk": "ok" })).unwrap();
        validate_builtin_card("ui://example/view", &json!([1, 2, 3])).unwrap();
        validate_builtin_card("ui://example/view", &Value::Null).unwrap();
    }

    #[test]
    fn plugin_prefixed_card_accepts_anything() {
        validate_builtin_card("plugin:foo:bar", &json!({ "whatever": true })).unwrap();
    }

    // ---------------- OverlayKindRegistry ----------------

    #[test]
    fn overlay_kind_registry_lookup_known_kinds() {
        let expected = [
            ("status", OVERLAY_STATUS_SCHEMA_VERSION),
            ("progress", OVERLAY_PROGRESS_SCHEMA_VERSION),
            ("eta", OVERLAY_ETA_SCHEMA_VERSION),
            ("now", OVERLAY_NOW_SCHEMA_VERSION),
            ("layout", OVERLAY_LAYOUT_SCHEMA_VERSION),
            ("file-viewer-nav", OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION),
            (
                "any_card_needs_input",
                OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION,
            ),
        ];

        for (kind, max_schema_version) in expected {
            let entry = OVERLAY_KIND_REGISTRY
                .lookup(kind)
                .unwrap_or_else(|| panic!("missing registry entry for {kind}"));
            assert_eq!(entry.kind, kind);
            assert_eq!(entry.max_schema_version, max_schema_version);
            assert_eq!(
                OVERLAY_KIND_REGISTRY.max_supported_schema_version(kind),
                Some(max_schema_version)
            );
        }
    }

    #[test]
    fn overlay_kind_registry_lookup_unknown_kind() {
        assert!(OVERLAY_KIND_REGISTRY.lookup("plugin:foo").is_none());
        assert!(OVERLAY_KIND_REGISTRY.lookup("").is_none());
    }

    #[test]
    fn overlay_kind_registry_validate_plugin_kind_opaque() {
        OVERLAY_KIND_REGISTRY
            .validate("plugin:foo", &json!({ "junk": true }))
            .unwrap();
        OVERLAY_KIND_REGISTRY
            .validate("plugin:foo", &json!({ "schemaVersion": 999 }))
            .unwrap();
    }

    #[test]
    fn overlay_kind_registry_validate_known_kinds_accept_and_reject() {
        let cases = [
            (
                "status",
                json!({ "state": "running" }),
                json!({ "state": 42 }),
            ),
            (
                "progress",
                json!({ "value": 0.5 }),
                json!({ "value": "fast" }),
            ),
            ("eta", json!({ "text": "5m" }), json!({ "text": null })),
            ("now", json!({ "text": "writing" }), json!({ "text": 7 })),
            (
                "layout",
                json!({ "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } } }),
                json!({ "positions": { "c": { "x": 10, "y": 0, "w": 4, "h": 3 } } }),
            ),
            (
                "file-viewer-nav",
                json!({
                    "tab": "code",
                    "folderPath": "/repo/src",
                    "selectedPath": null,
                    "diffSelected": null
                }),
                json!({
                    "tab": "history",
                    "folderPath": "/repo/src",
                    "selectedPath": null,
                    "diffSelected": null
                }),
            ),
            (
                "any_card_needs_input",
                json!({ "value": true }),
                json!({ "value": "yes" }),
            ),
        ];

        for (kind, valid, invalid) in cases {
            OVERLAY_KIND_REGISTRY.validate(kind, &valid).unwrap();
            assert!(matches!(
                OVERLAY_KIND_REGISTRY.validate(kind, &invalid),
                Err(CalmError::BadRequest(_))
            ));
        }
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

    // ---------------- Overlay: any_card_needs_input ----------------

    #[test]
    fn any_card_needs_input_happy_true() {
        validate_overlay_payload("any_card_needs_input", &json!({ "value": true })).unwrap();
    }

    #[test]
    fn any_card_needs_input_happy_false() {
        validate_overlay_payload("any_card_needs_input", &json!({ "value": false })).unwrap();
    }

    #[test]
    fn any_card_needs_input_with_schema_version() {
        validate_overlay_payload(
            "any_card_needs_input",
            &json!({ "schemaVersion": 1, "value": true }),
        )
        .unwrap();
    }

    #[test]
    fn any_card_needs_input_rejects_missing_value() {
        let err = validate_overlay_payload("any_card_needs_input", &json!({})).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn any_card_needs_input_rejects_wrong_type() {
        let err = validate_overlay_payload("any_card_needs_input", &json!({ "value": "yes" }))
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    // ---------------- Overlay: file-viewer-nav ----------------

    #[test]
    fn file_viewer_nav_happy_code_with_nulls() {
        validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap();
    }

    #[test]
    fn file_viewer_nav_happy_diff_with_paths() {
        validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "diff",
                "folderPath": "/repo/src",
                "selectedPath": "/repo/src/main.ts",
                "diffSelected": "src/main.ts"
            }),
        )
        .unwrap();
    }

    #[test]
    fn file_viewer_nav_rejects_missing_folder_path() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_tab() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "history",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn file_viewer_nav_rejects_wrong_selected_path_type() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": 42,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_field() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null,
                "extra": true
            }),
        )
        .unwrap_err();
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

    // ---------------- schemaVersion: payload_schema_version helper ----------------

    #[test]
    fn payload_schema_version_defaults_to_one_when_absent() {
        assert_eq!(payload_schema_version(&json!({})), 1);
        assert_eq!(payload_schema_version(&json!({ "other": "field" })), 1);
        assert_eq!(payload_schema_version(&Value::Null), 1);
    }

    #[test]
    fn payload_schema_version_returns_value_when_present() {
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": 1 })), 1);
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": 7 })), 7);
    }

    #[test]
    fn payload_schema_version_defaults_when_wrong_type() {
        // Non-integer values are not migration markers — fall back to 1 so
        // downstream code can still read the (mis-typed) payload while the
        // validator rejects it on the write boundary.
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": "1" })), 1);
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": null })), 1);
    }

    // ---------------- schemaVersion: card validators ----------------

    #[test]
    fn terminal_accepts_missing_schema_version() {
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1" })).unwrap();
    }

    #[test]
    fn terminal_accepts_matching_schema_version() {
        validate_builtin_card(
            "terminal",
            &json!({ "schemaVersion": 1, "terminal_id": "t1" }),
        )
        .unwrap();
    }

    #[test]
    fn terminal_rejects_unknown_schema_version() {
        let err = validate_builtin_card(
            "terminal",
            &json!({ "schemaVersion": 2, "terminal_id": "t1" }),
        )
        .unwrap_err();
        let CalmError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("schemaVersion"), "msg = {msg}");
        assert!(msg.contains("terminal"), "msg = {msg}");
        assert!(msg.contains('2'), "msg = {msg}");
    }

    #[test]
    fn codex_accepts_missing_schema_version() {
        validate_builtin_card("codex", &json!({ "any": "thing" })).unwrap();
    }

    #[test]
    fn codex_accepts_matching_schema_version() {
        validate_builtin_card("codex", &json!({ "schemaVersion": 1, "any": "thing" })).unwrap();
    }

    #[test]
    fn codex_rejects_unknown_schema_version() {
        let err = validate_builtin_card("codex", &json!({ "schemaVersion": 99, "any": "thing" }))
            .unwrap_err();
        let CalmError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("codex"), "msg = {msg}");
    }

    // ---------------- Card: wave-report (issue #229 PR B) ----------------

    #[test]
    fn wave_report_happy() {
        validate_builtin_card(
            "wave-report",
            &json!({ "schemaVersion": 1, "summary": "", "body": "# Goal\n" }),
        )
        .unwrap();
    }

    #[test]
    fn wave_report_accepts_missing_schema_version() {
        // Missing schemaVersion is treated as v1 (every kernel-owned
        // kind today is v1, so absent-as-1 stays unambiguous).
        validate_builtin_card(
            "wave-report",
            &json!({ "summary": "hi", "body": "# Done\n" }),
        )
        .unwrap();
    }

    #[test]
    fn wave_report_rejects_missing_summary() {
        let err = validate_builtin_card(
            "wave-report",
            &json!({ "schemaVersion": 1, "body": "# Goal" }),
        )
        .unwrap_err();
        let CalmError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("summary"), "msg = {msg}");
    }

    #[test]
    fn wave_report_rejects_missing_body() {
        let err = validate_builtin_card(
            "wave-report",
            &json!({ "schemaVersion": 1, "summary": "x" }),
        )
        .unwrap_err();
        let CalmError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("body"), "msg = {msg}");
    }

    #[test]
    fn wave_report_rejects_wrong_field_type() {
        let err = validate_builtin_card(
            "wave-report",
            &json!({ "schemaVersion": 1, "summary": 42, "body": "x" }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn wave_report_rejects_unknown_schema_version() {
        let err = validate_builtin_card(
            "wave-report",
            &json!({ "schemaVersion": 2, "summary": "", "body": "" }),
        )
        .unwrap_err();
        let CalmError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("wave-report"), "msg = {msg}");
        assert!(msg.contains('2'), "msg = {msg}");
    }

    #[test]
    fn wave_report_tolerates_unknown_fields() {
        // Forward-compat: extra fields are passed through (serde
        // ignores by default). A v2 that adds e.g. `lastWriter` lands
        // without an old-binary error.
        validate_builtin_card(
            "wave-report",
            &json!({
                "schemaVersion": 1,
                "summary": "",
                "body": "x",
                "futureField": "tolerated"
            }),
        )
        .unwrap();
    }

    // ---------------- schemaVersion: overlay validators ----------------

    #[test]
    fn status_accepts_matching_schema_version() {
        validate_overlay_payload("status", &json!({ "schemaVersion": 1, "state": "running" }))
            .unwrap();
    }

    #[test]
    fn status_rejects_unknown_schema_version() {
        let err = validate_overlay_payload(
            "status",
            &json!({ "schemaVersion": 99, "state": "running" }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("status")));
    }

    #[test]
    fn progress_accepts_matching_schema_version() {
        validate_overlay_payload("progress", &json!({ "schemaVersion": 1, "value": 0.5 })).unwrap();
    }

    #[test]
    fn progress_rejects_unknown_schema_version() {
        let err =
            validate_overlay_payload("progress", &json!({ "schemaVersion": 2, "value": 0.5 }))
                .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("schemaVersion")));
    }

    #[test]
    fn eta_accepts_matching_schema_version() {
        validate_overlay_payload("eta", &json!({ "schemaVersion": 1, "text": "5m" })).unwrap();
    }

    #[test]
    fn now_accepts_matching_schema_version() {
        validate_overlay_payload("now", &json!({ "schemaVersion": 1, "text": "writing" })).unwrap();
    }

    #[test]
    fn layout_accepts_matching_schema_version() {
        validate_overlay_payload(
            "layout",
            &json!({
                "schemaVersion": 1,
                "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } }
            }),
        )
        .unwrap();
    }

    #[test]
    fn layout_rejects_unknown_schema_version() {
        let err =
            validate_overlay_payload("layout", &json!({ "schemaVersion": 9, "positions": {} }))
                .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("schemaVersion")));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_schema_version() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 9,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("schemaVersion")));
    }

    // ---------------- schemaVersion: plugin-owned overlay passthrough ----------------

    #[test]
    fn plugin_overlay_passthrough_with_arbitrary_schema_version() {
        // A plugin-defined overlay kind carries whatever payload its author
        // chose — we don't inspect `schemaVersion` for these, even if the
        // value would be rejected on a kernel-owned kind.
        validate_overlay_payload(
            "custom-plugin-kind",
            &json!({ "schemaVersion": 999, "anything": true }),
        )
        .unwrap();
        validate_overlay_payload(
            "ui://example/view",
            &json!({ "schemaVersion": "totally a string", "x": 1 }),
        )
        .unwrap();
    }

    // ---------------- schemaVersion: invalid type ----------------

    #[test]
    fn rejects_non_integer_schema_version_on_kernel_kinds() {
        let err = validate_overlay_payload(
            "status",
            &json!({ "schemaVersion": "1", "state": "running" }),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(ref m) if m.contains("schemaVersion")));
    }

    // ---------------- max_supported_overlay_schema_version ----------------

    #[test]
    fn max_supported_overlay_schema_version_kernel_kinds() {
        // Every kernel-owned overlay kind reports its compile-time version.
        assert_eq!(
            max_supported_overlay_schema_version("status"),
            Some(OVERLAY_STATUS_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("progress"),
            Some(OVERLAY_PROGRESS_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("eta"),
            Some(OVERLAY_ETA_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("now"),
            Some(OVERLAY_NOW_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("layout"),
            Some(OVERLAY_LAYOUT_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("file-viewer-nav"),
            Some(OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION)
        );
    }

    #[test]
    fn max_supported_overlay_schema_version_plugin_kinds_return_none() {
        // Plugin-defined kinds opt out — the read guard must not touch them.
        assert_eq!(max_supported_overlay_schema_version("custom-badge"), None);
        assert_eq!(
            max_supported_overlay_schema_version("ui://example/view"),
            None
        );
        assert_eq!(max_supported_overlay_schema_version(""), None);
    }
}
