//! Truth/substrate layer for the calm kernel (#679 PR2).

// Retained for PR6 `WorkerProvider` impls.
use calm_exec as _;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub mod card_kind;
pub mod card_role_cache;
pub mod db;
pub mod decision_gate;
pub mod event_bus;
pub mod events_prune;
pub mod mcp_auth;
pub mod model;
pub mod repo_identity;
pub mod role_gate;
pub mod session_projection_lookup;
pub mod session_projection_repo;
pub mod session_projection_row;
pub mod session_repo;
pub mod state;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
pub mod validation;
pub mod wave_cove_cache;
pub mod wave_fs_view;
pub mod wave_vcs;
pub mod wave_vcs_repo;
pub mod worker_flow_sink;

pub mod error;
pub use error::TruthError;

pub mod event {
    pub use crate::event_bus::{BroadcastEnvelope, EventBus, SubscribeFilter, SubscribeScope};
    pub use calm_types::event::*;
}

pub use calm_types::{ids, wave_fs_dto, wave_lifecycle, wave_report, worker};
