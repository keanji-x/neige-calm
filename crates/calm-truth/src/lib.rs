//! Truth/substrate layer for the calm kernel.

// Retained for `WorkerProvider` impls.
use calm_exec as _;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub mod area_folder_claim;
#[cfg(feature = "fixtures")]
pub mod capture_test_seam;
pub mod card_kind;
pub mod card_role_cache;
pub mod db;
pub mod decision_gate;
pub mod event_bus;
pub mod events_prune;
pub mod mcp_auth;
pub mod model;
pub mod readable_error_text;
pub mod role_gate;
pub mod session_projection_lookup;
pub mod session_projection_repo;
pub mod session_projection_row;
pub mod session_repo;
pub mod state;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
#[cfg(feature = "fixtures")]
pub mod test_seam;
pub mod track_area_cache;
pub mod track_fs_view;
pub mod track_vcs;
pub mod track_vcs_repo;
pub mod validation;
pub mod worker_flow_sink;

pub mod error;
pub use error::TruthError;

pub mod event {
    pub use crate::event_bus::{BroadcastEnvelope, EventBus, SubscribeFilter, SubscribeScope};
    pub use calm_types::event::*;
}

pub use calm_types::{ids, track_fs_dto, track_report, worker};
