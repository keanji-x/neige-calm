//! Harness vocabulary shared across crates (#679 PR1).
//!
//! The phase tag and persisted header constants live here; the phase tag is
//! referenced by `Event::HarnessPhaseChanged` and TS-exported.
//! The full `HarnessSnapshot` (and the harness state
//! machine it snapshots) stays in calm-server's `harness` module: it is
//! provider-side machinery, scheduled to move behind the calm-exec
//! provider boundary in #679 PR6.

/// Persisted harness header and failure vocabulary used by the codec and
/// transactional recovery guards. These are wire values, not runtime policy.
pub const HARNESS_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const HARNESS_MODE: &str = "harness";
pub const HARNESS_SYSTEM_ERROR_REASON: &str = "system_error";

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
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
