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

use crate::claude_planner::session::ClaudePlannerSession;
use crate::codex_appserver::{InputItem, Notification};
use crate::error::{CalmError, Result};
use crate::planner_model::TurnModelSelection;
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::{SharedCodexAppServer, TurnId};

#[derive(Clone)]
pub enum PlannerBackend {
    Codex(Arc<SharedCodexAppServer>),
    /// One Claude Planner session (design #1791 §5); it holds the Codex daemon only for the
    /// thread-keyed deletion seals.
    Claude(Arc<ClaudePlannerSession>),
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
            Self::Claude(session) => session.subscribe_notifications(),
        }
    }

    /// The Claude arm passes the chosen alias as `--model` (#1810); a selection it cannot run is
    /// refused here too, never dropped.
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
            Self::Claude(session) => {
                let model = crate::claude_planner::models::resolve(
                    selection.model.as_deref(),
                    selection.effort.as_deref(),
                )
                .map_err(|e| CalmError::BadRequest(e.to_string()))?;
                session.turn_start(thread_id, items, model, client_id).await
            }
        }
    }

    /// Whether a queued entry can join the running turn (§5.9): the run loop checks this before
    /// it takes the entry out of the queue.
    pub fn supports_steer(&self) -> bool {
        match self {
            Self::Codex(_) => true,
            Self::Claude(_) => false,
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
            Self::Claude(_) => Err(CalmError::Internal(
                "a Claude Planner cannot steer; the run loop checks supports_steer first".into(),
            )),
        }
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        match self {
            Self::Codex(daemon) => daemon.turn_interrupt(thread_id, turn_id).await,
            Self::Claude(session) => session.turn_interrupt(thread_id, turn_id).await,
        }
    }

    pub async fn interrupt_active_turn(&self, thread_id: &str) -> Result<()> {
        match self {
            Self::Codex(daemon) => daemon.interrupt_active_turn(thread_id).await,
            Self::Claude(session) => session.interrupt_active_turn(thread_id).await,
        }
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        match self {
            Self::Codex(daemon) => daemon.active_turn_id_for_thread(thread_id),
            Self::Claude(session) => session.active_turn_id_for_thread(thread_id),
        }
    }

    pub fn provider(&self) -> AgentProvider {
        match self {
            Self::Codex(_) => AgentProvider::Codex,
            Self::Claude(_) => AgentProvider::Claude,
        }
    }

    /// The registry installed the harness: a Claude session may start turns from now on.
    pub fn mark_installed(&self) {
        match self {
            Self::Codex(_) => {}
            Self::Claude(session) => session.mark_installed(),
        }
    }

    /// The Codex daemon, for the thread-keyed deletion seals and the Codex-only config and
    /// model-catalog reads.
    pub fn codex(&self) -> &Arc<SharedCodexAppServer> {
        match self {
            Self::Codex(daemon) => daemon,
            Self::Claude(session) => session.codex(),
        }
    }
}
