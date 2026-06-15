//! Wave-report payload vocabulary (#679 PR1).
//!
//! [`WaveReportPayload`] is the Tier-A persisted card payload + TS-exported
//! wire type, so it lives here. The persist boundary (`persist_report`,
//! CRDT plumbing, REST/MCP resolvers) stays in calm-server's `wave_report`
//! module, which re-exports this type.

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
    /// [`crate::card_kind::WaveReportCardHandler`] and the matching
    /// frontend zod schema in
    /// `web/src/api/schemas.ts`.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Canonical "wave was just minted; spec hasn't run yet" payload.
    /// Used by `routes::waves::create_wave` (PR B). Historical
    /// migration seeds stay frozen; freshly-minted waves use this copy.
    pub fn initial() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            summary: String::new(),
            body: "# 概要\n\n_Spec agent 会在第一次 turn 时填这里。_\n".to_string(),
        }
    }
}
