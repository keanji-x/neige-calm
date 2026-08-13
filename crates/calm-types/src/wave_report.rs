//! Wave-report payload vocabulary (#679 PR1).
//!
//! [`WaveReportPayload`] is the Tier-A persisted card payload + TS-exported
//! wire type, so it lives here. The persist boundary (`persist_report`,
//! CRDT plumbing, REST/MCP resolvers) stays in calm-server's `wave_report`
//! module, which re-exports this type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// A derived, addressable slice of a wave report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct ReportBlock {
    pub id: String,
    pub kind: String,
    pub rev: u32,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

/// The payload persisted in a wave-report card's `payload` JSON column.
///
/// Wire shape (camelCase to match the rest of the kernel's payloads):
///
/// ```json
/// {
///   "schemaVersion": 3,
///   "docRev": 7,
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct WaveReportPayload {
    /// Tier A persistence contract — see
    /// `WAVE_REPORT_PAYLOAD_SCHEMA_VERSION` in calm-truth's
    /// `validation.rs`. `3` since #979 added document-wide optimistic
    /// concurrency; blocks remain authoritative and `body` is their
    /// flat projection. v1/v2 rows remain readable and are lazily
    /// upgraded at the next persist via the CRDT-layer migrator
    /// (`ReportDoc::ensure_blocks_layout`).
    pub schema_version: u32,
    /// Document-wide optimistic-concurrency revision. This is mirrored
    /// from the authoritative CRDT root and increments after every
    /// successful report persist (whole-document or block-level).
    #[serde(default)]
    #[schema(required = true)]
    pub doc_rev: u64,
    /// One-line summary used by sidebars / wave-list previews. Empty
    /// string is valid (means "spec agent has not produced a summary
    /// yet"); the field stays a required `String` per the
    /// [[required-over-option]] rule.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
    /// Block mirror of the authoritative CRDT block map (#960 PR2).
    /// Since schema v2 the CRDT `blocks`/`order` layout is the source
    /// of truth; this JSON field and `body` are both projections the
    /// persist boundary rewrites on every write. v1 rows may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<ReportBlock>>,
}

impl WaveReportPayload {
    /// Current schema version. Bumping this is a Tier A breaking
    /// change — the same PR must also extend
    /// [`crate::card_kind::WaveReportCardHandler`] and the matching
    /// frontend zod schema in
    /// `web/src/api/schemas.ts`.
    pub const SCHEMA_VERSION: u32 = 3;

    pub fn new(summary: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            doc_rev: 0,
            summary: summary.into(),
            body: body.into(),
            blocks: None,
        }
    }

    /// Canonical "wave was just minted; spec hasn't run yet" payload.
    /// Used by `routes::waves::create_wave` (PR B). Historical
    /// migration seeds stay frozen; freshly-minted waves use this copy.
    pub fn initial() -> Self {
        Self::new("", "# 概要\n\n_Spec agent 会在第一次 turn 时填这里。_\n")
    }
}
