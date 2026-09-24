//! The runtime a Planner harness drives its turns through.
//!
//! Every provider-coupled call the run loop makes on a Planner turn goes through
//! [`PlannerBackend`]. Calls with no provider-neutral meaning (thread seals, the Codex
//! config and model catalog) reach the Codex daemon through [`PlannerBackend::codex`].
//!
//! `client_id` on [`PlannerBackend::turn_start`] and [`PlannerBackend::turn_steer`] is the
//! projection row's key; codex hands it back as `item.clientId`.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::codex_appserver::{InputItem, Notification};
use crate::error::Result;
use crate::planner_model::TurnModelSelection;
use crate::shared_codex_appserver::{SharedCodexAppServer, TurnId};

#[derive(Clone)]
pub enum PlannerBackend {
    Codex(Arc<SharedCodexAppServer>),
}

impl From<Arc<SharedCodexAppServer>> for PlannerBackend {
    fn from(daemon: Arc<SharedCodexAppServer>) -> Self {
        Self::Codex(daemon)
    }
}

impl PlannerBackend {
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        match self {
            Self::Codex(daemon) => daemon.subscribe_notifications(),
        }
    }

    pub async fn turn_start(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_id: &str,
    ) -> Result<TurnId> {
        match self {
            Self::Codex(daemon) => {
                daemon
                    .turn_start(thread_id, items, selection, Some(client_id))
                    .await
            }
        }
    }

    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        items: Vec<InputItem>,
        client_id: &str,
    ) -> Result<TurnId> {
        match self {
            Self::Codex(daemon) => {
                daemon
                    .turn_steer(thread_id, expected_turn_id, items, Some(client_id))
                    .await
            }
        }
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        match self {
            Self::Codex(daemon) => daemon.turn_interrupt(thread_id, turn_id).await,
        }
    }

    pub async fn interrupt_active_turn(&self, thread_id: &str) -> Result<()> {
        match self {
            Self::Codex(daemon) => daemon.interrupt_active_turn(thread_id).await,
        }
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        match self {
            Self::Codex(daemon) => daemon.active_turn_id_for_thread(thread_id),
        }
    }

    /// The Codex daemon, for the thread-keyed deletion seals and the Codex-only config and
    /// model-catalog reads.
    pub fn codex(&self) -> &Arc<SharedCodexAppServer> {
        match self {
            Self::Codex(daemon) => daemon,
        }
    }
}
