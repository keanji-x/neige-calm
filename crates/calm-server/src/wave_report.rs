//! Issue #229 PR B — wave-report card payload + MCP-tool support helpers.
//!
//! The wave-report card is a kernel-owned card minted at wave-create time
//! (plus backfilled for legacy waves via migration 0014). Its payload is a
//! single Markdown document the spec agent maintains via three MCP tools
//! that mimic codex's native Read/Edit/Write file tools 1:1:
//!
//!   * `calm.report.read`  — fetch current body + summary
//!   * `calm.report.write` — wholesale replace (like codex `Write`)
//!   * `calm.report.edit`  — string replacement (like codex `Edit`;
//!     `old_string` must be unique unless `replace_all = true`)
//!
//! Storage shape is intentionally one big Markdown string rather than a
//! `Vec<Section>` — sections are derived at render time by splitting at
//! H1 headings (`^# `). This keeps the spec agent's mental model simple
//! (it's editing a Markdown file), keeps the wire shape stable across
//! UI iterations on the section vocabulary, and avoids a second
//! storage-shape negotiation if the section list ever needs to change.
//!
//! ## Schema versioning (Tier A persistence contract)
//!
//! See `docs/upgrade-stability.md`. The struct carries `schema_version`
//! explicitly + matches it against
//! [`crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION`] at every
//! write boundary. v1 is the only shape that has ever existed.
//!
//! ## Field rationale ([[required-over-option]])
//!
//! `summary` and `body` are required `String` (not `Option<String>`):
//! every callsite must commit to a value. An empty `summary` is a valid
//! value ("the agent hasn't written a one-liner yet"); the `Option`
//! shape would have introduced two indistinguishable absent-states
//! (`null` vs missing) for no information gain. `WaveReportPayload::initial()`
//! seeds the canonical "agent hasn't run yet" defaults.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// The payload persisted in a wave-report card's `payload` JSON column.
///
/// Wire shape (camelCase to match the rest of the kernel's payloads):
///
/// ```json
/// {
///   "schemaVersion": 1,
///   "summary": "Refactored the dispatcher into a typed actor",
///   "body": "# Goal\n\nReplace the ad-hoc loop with…\n\n# Progress\n..."
/// }
/// ```
///
/// `summary` is the one-line previewable in sidebars / list views;
/// `body` is the Markdown source the WaveReportCard renders. The
/// frontend derives sections from `body` by splitting on H1 headings;
/// the storage layer does not impose a section vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
#[serde(rename_all = "camelCase")]
pub struct WaveReportPayload {
    /// Tier A persistence contract — see
    /// [`crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION`].
    /// Always `1` today; a future v2 would bump this constant + add a
    /// migrator next to it in `validation.rs`.
    pub schema_version: u32,
    /// One-line summary used by sidebars / wave-list previews. Empty
    /// string is valid (means "spec agent has not produced a summary
    /// yet"); the field stays a required `String` per the
    /// [[required-over-option]] rule.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
}

impl WaveReportPayload {
    /// Current schema version. Bumping this is a Tier A breaking
    /// change — the same PR must also extend
    /// [`crate::validation::validate_card_payload`]'s wave-report arm
    /// and the matching frontend zod schema in
    /// `web/src/api/schemas.ts`.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Canonical "wave was just minted; spec hasn't run yet" payload.
    /// Used by `routes::waves::create_wave` (PR B) and mirrored
    /// verbatim by the SQL string in migration 0014 — keep them in
    /// sync if the placeholder copy ever changes.
    pub fn initial() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            summary: String::new(),
            body: "# Goal\n\n_The spec agent will fill this in._\n".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_carries_current_schema_version() {
        let p = WaveReportPayload::initial();
        assert_eq!(p.schema_version, WaveReportPayload::SCHEMA_VERSION);
        assert!(p.summary.is_empty());
        assert!(p.body.contains("# Goal"));
        assert!(p.body.ends_with('\n'));
    }

    #[test]
    fn serde_round_trip_camelcase_wire() {
        let p = WaveReportPayload {
            schema_version: 1,
            summary: "hi".to_string(),
            body: "# A\n\nb\n".to_string(),
        };
        let v = serde_json::to_value(&p).unwrap();
        // Wire shape: camelCase keys. A drift here would break the
        // frontend's zod schema silently — pin via this test.
        assert_eq!(
            v,
            json!({
                "schemaVersion": 1,
                "summary": "hi",
                "body": "# A\n\nb\n",
            })
        );
        let back: WaveReportPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        // No `body`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
            "schemaVersion": 1,
            "summary": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("body"), "got: {err}");

        // No `summary`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
            "schemaVersion": 1,
            "body": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("summary"), "got: {err}");
    }

    #[test]
    fn initial_matches_migration_seed_body() {
        // Migration 0014 hard-codes the same placeholder string; if
        // this assertion fails the migration's INSERT and `initial()`
        // have diverged — fix one to match the other so backfilled
        // and freshly-minted waves render identically.
        let p = WaveReportPayload::initial();
        assert_eq!(p.body, "# Goal\n\n_The spec agent will fill this in._\n");
    }
}
