//! The runtime a Planner harness drives its turns through.
//!
//! Every provider-coupled call the run loop makes on a Planner turn goes through
//! [`PlannerBackend`], including model resolution and shutdown. The deletion seals are not a
//! provider's: they live in the shared [`ThreadSeals`].
//!
//! `client_id` on [`PlannerBackend::turn_start`] and [`PlannerBackend::turn_steer`] is the
//! projection row's key; codex hands it back as `item.clientId`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;

use crate::claude_planner::session::ClaudePlannerSession;
use crate::claude_planner::spawn::ResumeTruncation;
use crate::claude_planner::wiring::{ClaudePlannerRow, ClaudePlannerWiring};
use crate::codex_appserver::InputItem;
use crate::db::Repo;
use crate::db::TranscriptRow;
use crate::error::{CalmError, Result};
use crate::harness::codex_events::CodexEvents;
use crate::harness::codex_selection;
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use crate::harness::planner_event::PlannerEvent;
use crate::planner_model::TurnModelSelection;
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::{SharedCodexAppServer, TurnId};
use crate::thread_seals::ThreadSeals;

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
    /// The provider declared a hold; recovery is an explicit provider observation, not a timer.
    #[error("{error}")]
    AwaitingRecovery { error: CalmError, reader: String },
}

/// A rewind the provider applies when the conversation's next turn starts (#1923). Its arms are
/// private like [`PlannerBackend`]'s: the run loop stores, passes and clears it, never reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendRewind(RewindArm);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
enum RewindArm {
    /// `thread/revert {threadId, beforeTurnId}` before the next `turn/start`.
    Codex { before_turn_id: String },
    /// `--resume-session-at` / `--resume-drops-turn` on the next spawn.
    Claude { resume_at: Uuid, drops_turn: Uuid },
}

/// What a provider is told about the turn a rewind removes.
pub struct RewindTarget<'a> {
    pub turn_id: &'a str,
    /// The client id of the turn's first user message (its projection key), when it has one.
    pub prompt_client_id: Option<&'a str>,
    /// The rows of the turn before it, or `None` when it is the thread's first.
    pub previous_turn: Option<&'a [TranscriptRow]>,
}

/// The provider a Planner harness runs on. Its arms are private to this module, so code outside it
/// cannot name or match a provider and goes through the methods below.
#[derive(Clone)]
pub struct PlannerBackend(Arm);

#[derive(Clone)]
enum Arm {
    Acp(Arc<crate::acp_planner::session::AcpPlannerSession>),
    Codex(Arc<SharedCodexAppServer>),
    /// One Claude Planner session (design #1791 §5).
    Claude(Arc<ClaudePlannerSession>),
}

impl From<Arc<SharedCodexAppServer>> for PlannerBackend {
    fn from(daemon: Arc<SharedCodexAppServer>) -> Self {
        Self(Arm::Codex(daemon))
    }
}

impl PlannerBackend {
    /// The backend for `provider`. A Claude session only is opened: nothing is minted or spawned
    /// before the harness is installed and its first turn runs. `seals` is the server's one
    /// registry, the one the Codex daemon already consults.
    pub async fn open(
        provider: AgentProvider,
        daemon: Arc<SharedCodexAppServer>,
        seals: Arc<ThreadSeals>,
        claude: &ClaudePlannerWiring,
        repo: Arc<dyn Repo>,
        row: ClaudePlannerRow<'_>,
    ) -> Result<Self> {
        Ok(match provider {
            AgentProvider::Codex => daemon.into(),
            AgentProvider::OpenCode => {
                let acp = crate::acp_planner::wiring::AcpPlannerWiring {
                    host: claude.acp.clone(),
                    plugin: claude.plugin.clone(),
                };
                Self(Arm::Acp(
                    acp.open_session(provider, repo, seals, row).await?,
                ))
            }
            AgentProvider::Claude => {
                Self(Arm::Claude(claude.open_session(repo, seals, row).await?))
            }
        })
    }

    /// Provider-owned suspension; the generic harness does not infer authentication policy.
    pub(crate) fn issuance_hold(&self) -> Option<String> {
        match &self.0 {
            Arm::Codex(daemon) => daemon.authentication_hold(),
            Arm::Claude(_) => None,
        }
    }

    pub fn subscribe_events(&self) -> PlannerEvents {
        PlannerEvents(match &self.0 {
            Arm::Acp(session) => EventSource::Acp(session.subscribe_events()),
            Arm::Codex(daemon) => EventSource::Codex(CodexEvents::subscribe(daemon)),
            Arm::Claude(session) => EventSource::Claude(session.subscribe_events()),
        })
    }

    /// Decide how this provider removes `target` from the conversation, changing nothing. A refusal
    /// is a `Conflict` the reader is shown. Codex needs no preparation; Claude needs an anchor before
    /// the turn and a dry run of the cut.
    pub async fn prepare_rewind(
        &self,
        thread_id: &str,
        target: RewindTarget<'_>,
    ) -> Result<BackendRewind> {
        if let Some(reader) = self.issuance_hold() {
            return Err(CalmError::Conflict(reader));
        }
        match &self.0 {
            Arm::Acp(_) => Err(CalmError::Conflict(
                "ACP history replacement is unsupported".into(),
            )),
            Arm::Codex(_) => Ok(BackendRewind(RewindArm::Codex {
                before_turn_id: target.turn_id.to_string(),
            })),
            Arm::Claude(session) => {
                let truncation = crate::claude_planner::rewind::truncation(
                    target.previous_turn,
                    target.prompt_client_id,
                )?;
                session.check_truncation(thread_id, &truncation).await?;
                Ok(BackendRewind(RewindArm::Claude {
                    resume_at: truncation.at,
                    drops_turn: truncation.drops_turn,
                }))
            }
        }
    }

    /// Whether the deletion fence has sealed `thread_id`.
    pub fn thread_sealed(&self, thread_id: &str) -> bool {
        match &self.0 {
            Arm::Acp(session) => session.thread_sealed(thread_id),
            Arm::Codex(daemon) => daemon.thread_seals().is_sealed(thread_id),
            Arm::Claude(session) => session.thread_sealed(thread_id),
        }
    }

    /// The Claude arm passes the stored model and effort as `--model=` and `--effort=` (#1810,
    /// #1822 6′); no catalog is consulted here, and the CLI judges the model. `rewind` is applied
    /// first: Codex reverts the thread before `turn/start`, Claude cuts the session in the spawn.
    pub async fn turn_start(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_id: &str,
        rewind: Option<&BackendRewind>,
    ) -> std::result::Result<TurnId, TurnStartFailure> {
        if let Some(reader) = self.issuance_hold() {
            return Err(TurnStartFailure::AwaitingRecovery {
                error: CalmError::ServiceUnavailable(reader.clone()),
                reader,
            });
        }
        let rewind = rewind.map(|rewind| &rewind.0);
        match &self.0 {
            Arm::Acp(session) => {
                if rewind.is_some() {
                    return Err(mismatched_rewind());
                }
                session
                    .turn_start(thread_id, items, selection, client_id)
                    .await
            }
            Arm::Codex(daemon) => {
                match rewind {
                    None => {}
                    Some(RewindArm::Codex { before_turn_id }) => {
                        revert_codex_thread(daemon, thread_id, before_turn_id).await?
                    }
                    Some(RewindArm::Claude { .. }) => return Err(mismatched_rewind()),
                }
                daemon
                    .turn_start(thread_id, items, selection, Some(client_id))
                    .await
                    // Codex answering with a refusal is the one failure known to be its answer.
                    .map_err(codex_turn_start_failure)
            }
            Arm::Claude(session) => {
                let truncation = match rewind {
                    None => None,
                    Some(RewindArm::Claude {
                        resume_at,
                        drops_turn,
                    }) => Some(ResumeTruncation {
                        at: *resume_at,
                        drops_turn: *drops_turn,
                    }),
                    Some(RewindArm::Codex { .. }) => return Err(mismatched_rewind()),
                };
                session
                    .turn_start(thread_id, items, selection, client_id, truncation.as_ref())
                    .await
            }
        }
    }

    /// Whether a queued entry can join the running turn (§5.9): the run loop checks this before
    /// it takes the entry out of the queue.
    pub fn supports_steer(&self) -> bool {
        match &self.0 {
            Arm::Codex(_) => true,
            Arm::Claude(_) | Arm::Acp(_) => false,
        }
    }

    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        items: Vec<InputItem>,
        client_id: &str,
    ) -> Result<TurnId> {
        match &self.0 {
            Arm::Codex(daemon) => {
                daemon
                    .turn_steer(thread_id, expected_turn_id, items, Some(client_id))
                    .await
            }
            Arm::Claude(_) | Arm::Acp(_) => Err(CalmError::Internal(
                "a Claude Planner cannot steer; the run loop checks supports_steer first".into(),
            )),
        }
    }

    pub async fn compact_start(&self, thread_id: &str) -> Result<()> {
        match &self.0 {
            Arm::Codex(daemon) => daemon.thread_compact_start(thread_id).await,
            Arm::Claude(_) | Arm::Acp(_) => Err(CalmError::BadRequest(
                "Manual context compaction is available for Codex conversations.".into(),
            )),
        }
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        match &self.0 {
            Arm::Acp(session) => session.turn_interrupt(thread_id, turn_id).await,
            Arm::Codex(daemon) => daemon.turn_interrupt(thread_id, turn_id).await,
            Arm::Claude(session) => session.turn_interrupt(thread_id, turn_id).await,
        }
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        match &self.0 {
            Arm::Acp(session) => session.active_turn_id_for_thread(thread_id),
            Arm::Codex(daemon) => daemon.active_turn_id_for_thread(thread_id),
            Arm::Claude(session) => session.active_turn_id_for_thread(thread_id),
        }
    }

    pub fn provider(&self) -> AgentProvider {
        match &self.0 {
            Arm::Acp(session) => session.provider(),
            Arm::Codex(_) => AgentProvider::Codex,
            Arm::Claude(_) => AgentProvider::Claude,
        }
    }

    /// What this turn runs under, read from the card as it stands now. The provider decides what
    /// else it asks and how each failure is refused; `source` answers the provider-neutral reads.
    pub(crate) async fn resolve_selection(
        &self,
        source: &impl SelectionSource,
    ) -> std::result::Result<TurnModelSelection, IssuanceRefusal> {
        match &self.0 {
            Arm::Acp(session) => {
                session
                    .host()
                    .configured(&session.provider())
                    .map_err(|e| IssuanceRefusal::needs_a_choice(e.to_string(), e.to_string()))?;
                let payload = source.card_payload().await?;
                let selected = crate::planner_model::CardModelSelection::from_payload(&payload)
                    .map_err(|e| IssuanceRefusal::needs_a_choice(e.to_string(), e.to_string()))?;
                Ok(TurnModelSelection {
                    model: selected.model,
                    effort: selected.reasoning_effort,
                })
            }
            Arm::Codex(daemon) => {
                let payload = source.card_payload().await?;
                codex_selection::resolve(daemon, source, &payload).await
            }
            // #1810: a Claude Planner reads its card like Codex does, but nothing is asked of
            // Codex. A server started without its config stops it first.
            Arm::Claude(session) => {
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
        match &self.0 {
            Arm::Acp(session) => return session.shutdown().await,
            // #1791 §5.1: the running turn is recorded `Interrupted` and this waits for `stop`,
            // whether or not a turn runs or a thread is known.
            Arm::Claude(session) => {
                if let Err(e) = session.shutdown().await {
                    tracing::warn!(
                        worker_session_id = %worker_session_id,
                        error = %e,
                        "planner harness shutdown: the Claude Planner stop did not confirm"
                    );
                    interrupt_error = Some(e);
                }
            }
            Arm::Codex(daemon) => {
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
        match &self.0 {
            Arm::Codex(_) => {}
            Arm::Acp(session) => session.mark_installed(),
            Arm::Claude(session) => session.mark_installed(),
        }
    }

    /// Fixtures only: a backend around a Claude session a test opened itself.
    #[cfg(feature = "fixtures")]
    pub fn claude_for_test(session: Arc<ClaudePlannerSession>) -> Self {
        Self(Arm::Claude(session))
    }

    /// Fixtures only: the Claude session, for its interleaving hooks.
    #[cfg(feature = "fixtures")]
    pub fn claude_session_for_test(&self) -> Option<Arc<ClaudePlannerSession>> {
        match &self.0 {
            Arm::Claude(session) => Some(Arc::clone(session)),
            Arm::Codex(_) | Arm::Acp(_) => None,
        }
    }
}

/// One harness's subscription to its provider's events, from
/// [`PlannerBackend::subscribe_events`], with `broadcast`'s receive semantics: `Lagged` when it
/// fell behind, `Closed` once the provider's sender is gone.
pub struct PlannerEvents(EventSource);

/// Private like [`Arm`]: code outside this module receives events without naming a provider.
enum EventSource {
    Acp(broadcast::Receiver<PlannerEvent>),
    Codex(CodexEvents),
    Claude(broadcast::Receiver<PlannerEvent>),
}

impl PlannerEvents {
    /// Cancel safe, like the `broadcast::Receiver::recv` it wraps: the run loop selects on it.
    pub async fn recv(&mut self) -> std::result::Result<PlannerEvent, RecvError> {
        match &mut self.0 {
            EventSource::Codex(events) => events.recv().await,
            EventSource::Claude(events) | EventSource::Acp(events) => events.recv().await,
        }
    }
}

fn codex_turn_start_failure(error: CalmError) -> TurnStartFailure {
    match error {
        CalmError::CodexRefused(message) => {
            if let Some(failure) = provider::codex::AuthenticationFailure::from_message(&message)
                .or_else(|| provider::codex::AuthenticationFailure::from_code(&message))
            {
                TurnStartFailure::AwaitingRecovery {
                    error: CalmError::CodexRefused(failure.code().into()),
                    reader: format!(
                        "{} Your message is still queued.",
                        crate::codex_authentication::SIGN_IN_REQUIRED
                    ),
                }
            } else {
                TurnStartFailure::Refused {
                    error: CalmError::CodexRefused(message),
                    reader: "codex refused to start a turn for this conversation, so \
                             your message has not been sent. If you changed the model \
                             recently, it may not be one this account can run — try \
                             another."
                        .into(),
                }
            }
        }
        error => TurnStartFailure::Transient(error),
    }
}

/// A rewind recorded for the other provider: the runtime's provider never changes, so this is a
/// corrupt snapshot that no retry clears.
fn mismatched_rewind() -> TurnStartFailure {
    TurnStartFailure::Refused {
        error: CalmError::Internal("a pending rewind names another provider".into()),
        reader: "this conversation holds an edit for another provider, so your message has not \
                 been sent. Reset the conversation to continue."
            .into(),
    }
}

/// `thread/revert` before the turn that follows a rewind. A turn the thread no longer has was
/// reverted by an earlier attempt, so that answer lets the turn start.
async fn revert_codex_thread(
    daemon: &SharedCodexAppServer,
    thread_id: &str,
    before_turn_id: &str,
) -> std::result::Result<(), TurnStartFailure> {
    match daemon.thread_revert(thread_id, before_turn_id).await {
        Ok(()) => Ok(()),
        Err(error @ CalmError::CodexRefused(_)) => Err(TurnStartFailure::Refused {
            error,
            reader: "codex refused to remove the edited turn from this conversation, so your \
                     message has not been sent. Reset the conversation to continue."
                .into(),
        }),
        Err(error) => Err(TurnStartFailure::Transient(error)),
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
