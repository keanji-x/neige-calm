//! The runtime a Planner harness drives its turns through.
//!
//! Every provider-coupled call the run loop makes on a Planner turn goes through
//! [`PlannerBackend`], including model resolution and shutdown. Only the thread-keyed deletion
//! seals still reach the Codex daemon through [`PlannerBackend::codex`].
//!
//! `client_id` on [`PlannerBackend::turn_start`] and [`PlannerBackend::turn_steer`] is the
//! projection row's key; codex hands it back as `item.clientId`.

use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::claude_planner::session::ClaudePlannerSession;
use crate::claude_planner::wiring::{ClaudePlannerRow, ClaudePlannerWiring};
use crate::codex_appserver::{InputItem, Notification};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::codex_selection;
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use crate::planner_model::TurnModelSelection;
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::{SharedCodexAppServer, TurnId};

/// A provider's failure to start a turn, classified by the provider that raised it (#1981): the
/// run loop paces its retry and tells the reader from this, never from the error's shape.
#[derive(Debug, thiserror::Error)]
pub enum TurnStartFailure {
    /// Repeating it unchanged may work, so the reader is not told yet.
    #[error(transparent)]
    Transient(#[from] CalmError),
    /// Repeating it unchanged reproduces it until someone changes something; `reader` says so now.
    #[error("{error}")]
    Refused { error: CalmError, reader: String },
}

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
    /// The backend for `provider`. A Claude session only is opened: nothing is minted or spawned
    /// before the harness is installed and its first turn runs.
    pub async fn open(
        provider: AgentProvider,
        daemon: Arc<SharedCodexAppServer>,
        claude: &ClaudePlannerWiring,
        repo: Arc<dyn Repo>,
        row: ClaudePlannerRow<'_>,
    ) -> Result<Self> {
        Ok(match provider {
            AgentProvider::Codex => daemon.into(),
            AgentProvider::Claude => Self::Claude(claude.open_session(repo, daemon, row).await?),
        })
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        match self {
            Self::Codex(daemon) => daemon.subscribe_notifications(),
            Self::Claude(session) => session.subscribe_notifications(),
        }
    }

    /// The Claude arm passes the stored model and effort as `--model=` and `--effort=` (#1810,
    /// #1822 6′); no catalog is consulted here, and the CLI judges the model.
    pub async fn turn_start(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_id: &str,
    ) -> std::result::Result<TurnId, TurnStartFailure> {
        match self {
            Self::Codex(daemon) => daemon
                .turn_start(thread_id, items, selection, Some(client_id))
                .await
                // Codex answering with a refusal is the one failure known to be its answer.
                .map_err(|error| match error {
                    CalmError::CodexRefused(_) => TurnStartFailure::Refused {
                        error,
                        reader: "codex refused to start a turn for this conversation, so your \
                                 message has not been sent. If you changed the model recently, it \
                                 may not be one this account can run — try another."
                            .into(),
                    },
                    error => TurnStartFailure::Transient(error),
                }),
            Self::Claude(session) => {
                session
                    .turn_start(thread_id, items, selection, client_id)
                    .await
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

    /// What this turn runs under, read from the card as it stands now. The provider decides what
    /// else it asks and how each failure is refused; `source` answers the provider-neutral reads.
    pub(crate) async fn resolve_selection(
        &self,
        source: &impl SelectionSource,
    ) -> std::result::Result<TurnModelSelection, IssuanceRefusal> {
        match self {
            Self::Codex(daemon) => {
                let payload = source.card_payload().await?;
                codex_selection::resolve(daemon, source, &payload).await
            }
            // #1810: a Claude Planner reads its card like Codex does, but nothing is asked of
            // Codex. A server started without its config stops it first.
            Self::Claude(session) => {
                if let Err(error) = session.host().configured() {
                    return Err(IssuanceRefusal::needs_a_choice(
                        error.to_string(),
                        format!(
                            "{}. Your message is still queued and will be sent once the server runs with it.",
                            crate::claude_planner::config::unavailable_message()
                        ),
                    ));
                }
                let payload = source.card_payload().await?;
                // #1822 6′: no catalog is consulted at issue; the CLI judges the model it is given.
                crate::claude_planner::models::turn_selection(&payload)
                    .map_err(|(log, reader)| IssuanceRefusal::needs_a_choice(log, reader))
            }
        }
    }

    /// Stop this harness's turns. `Err` is the last step that failed, for strict callers; every
    /// failure is logged here. `last_turn_id` is read only when Codex has a thread to interrupt.
    pub async fn shutdown(
        &self,
        worker_session_id: &str,
        thread_id: Option<&str>,
        last_turn_id: &Mutex<Option<String>>,
    ) -> Result<()> {
        let mut interrupt_error = None;
        match self {
            // #1791 §5.1: the running turn is recorded `Interrupted` and this waits for `stop`,
            // whether or not a turn runs or a thread is known.
            Self::Claude(session) => {
                if let Err(e) = session.shutdown().await {
                    tracing::warn!(
                        worker_session_id = %worker_session_id,
                        error = %e,
                        "planner harness shutdown: the Claude Planner stop did not confirm"
                    );
                    interrupt_error = Some(e);
                }
            }
            Self::Codex(daemon) => {
                if let Some(thread_id) = thread_id {
                    let last_turn_id = last_turn_id.lock().await.clone();
                    interrupt_codex_thread(
                        daemon,
                        thread_id,
                        last_turn_id.as_deref(),
                        |step, e| {
                            match step {
                                CodexInterruptStep::ActiveTurn => tracing::warn!(
                                    thread_id,
                                    error = %e,
                                    "planner harness shutdown thread interrupt failed"
                                ),
                                CodexInterruptStep::FallbackTurn(last_turn_id) => tracing::warn!(
                                    thread_id,
                                    turn_id = %last_turn_id,
                                    error = %e,
                                    "planner harness shutdown last-known turn interrupt failed"
                                ),
                            }
                            interrupt_error = Some(e);
                        },
                    )
                    .await;
                }
            }
        }
        match interrupt_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The registry installed the harness: a Claude session may start turns from now on.
    pub fn mark_installed(&self) {
        match self {
            Self::Codex(_) => {}
            Self::Claude(session) => session.mark_installed(),
        }
    }

    /// Fixtures only: the Claude session, for its interleaving hooks.
    #[cfg(feature = "fixtures")]
    pub fn claude_session_for_test(&self) -> Option<Arc<ClaudePlannerSession>> {
        match self {
            Self::Claude(session) => Some(Arc::clone(session)),
            Self::Codex(_) => None,
        }
    }

    /// The Codex daemon, for the thread-keyed deletion seals.
    pub fn codex(&self) -> &Arc<SharedCodexAppServer> {
        match self {
            Self::Codex(daemon) => daemon,
            Self::Claude(session) => session.codex(),
        }
    }
}

/// Which step of [`interrupt_codex_thread`] failed.
pub(crate) enum CodexInterruptStep<'a> {
    /// Interrupting the turn the daemon has cached for the thread.
    ActiveTurn,
    /// Interrupting the caller's known turn, tried only when the daemon had none cached.
    FallbackTurn(&'a str),
}

/// Codex's shutdown interrupt for one thread: the cached active turn, then `fallback_turn_id`
/// when the daemon had no turn cached before the first step. Each failure goes to `on_error`
/// as it happens and does not stop the next step.
pub(crate) async fn interrupt_codex_thread(
    daemon: &SharedCodexAppServer,
    thread_id: &str,
    fallback_turn_id: Option<&str>,
    mut on_error: impl FnMut(CodexInterruptStep<'_>, CalmError),
) {
    let active_turn_id = daemon.active_turn_id_for_thread(thread_id);
    if let Err(e) = daemon.interrupt_active_turn(thread_id).await {
        on_error(CodexInterruptStep::ActiveTurn, e);
    }
    if active_turn_id.is_none()
        && let Some(turn_id) = fallback_turn_id
        && let Err(e) = daemon.turn_interrupt(thread_id, turn_id).await
    {
        on_error(CodexInterruptStep::FallbackTurn(turn_id), e);
    }
}
