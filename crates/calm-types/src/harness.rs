//! Harness vocabulary shared across crates (#679 PR1).
//!
//! Only [`HarnessPhaseTag`] lives here — it is referenced by
//! `Event::HarnessPhaseChanged` and TS-exported, so it belongs to the
//! vocabulary crate. The full `HarnessSnapshot` (and the harness state
//! machine it snapshots) stays in calm-server's `harness` module: it is
//! provider-side machinery, scheduled to move behind the calm-exec
//! provider boundary in #679 PR6.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum HarnessPhaseTag {
    PendingThreadStart,
    Idle,
    IssuingTurn,
    IssuingInterrupt,
    TurnRunning,
    TurnCompleted,
    Resumed,
    Wedged,
}
