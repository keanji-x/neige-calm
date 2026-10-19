//! The runtime a Planner harness drives its turns through.
//!
//! Every provider-coupled call the run loop makes on a Planner turn goes through
//! the owner-bound `IssueTurnHandle`, with model resolution on [`PlannerBackend`]. The deletion seals are not a
//! provider's: they live in the shared [`crate::thread_seals::ThreadSeals`].
//!
//! `input.correlation` on managed start and steer
//! retains the projection row's key; codex hands it back as `item.clientId`.

use provider::control::*;
mod control;
mod managed;
pub(super) use managed::IssueTurnHandle;
pub(crate) mod factory;
mod start;
use control::{codex_compact_submission, steer_outcome};
pub use start::StartDiagnostic;
pub(crate) use start::ordinary_start_result;
use start::{codex_start_result, revert_codex_thread};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::claude_planner::session::ClaudePlannerSession;
use crate::codex_appserver::{ApprovalRoute, TurnApprovals};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::codex_events::CodexEvents;
use crate::harness::codex_selection;
use crate::harness::held_requests::HeldRequestSender;
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use crate::harness::planner_event::PlannerEvent;
use crate::harness::run_recovery::{RunRecovery, RunRecoveryAction, RunRecoveryCause};
use crate::harness::{HarnessPhaseTag, HarnessSnapshot};
use crate::planner_model::TurnModelSelection;
use crate::session_projection_repo::{AgentProvider, WorkerSessionProjection};
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
    /// The provider declared a hold; recovery is an explicit provider observation, not a timer.
    #[error("{error}")]
    AwaitingRecovery { error: CalmError, reader: String },
}

pub type InterruptDiagnostic = Box<dyn std::error::Error + Send + Sync>;

/// A rewind the provider applies when the conversation's next turn starts (#1923). Its arms are
/// private like [`PlannerBackend`]'s: the run loop stores, passes and clears it, never reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendRewind(RewindArm);

use calm_types::harness::snapshot::PersistedRewind as RewindArm;

/// The provider a Planner harness runs on. Its arms are private to this module, so code outside it
/// cannot name or match a provider and goes through the methods below.
#[derive(Clone)]
pub struct PlannerBackend(Arm);

#[derive(Clone)]
enum Arm {
    Acp(Arc<crate::acp_planner::session::AcpPlannerSession>),
    Codex(Arc<SharedCodexAppServer>, Arc<CodexApprovals>),
    /// One Claude Planner session (design #1791 §5).
    Claude(Arc<ClaudePlannerSession>),
}

impl From<Arc<SharedCodexAppServer>> for PlannerBackend {
    fn from(daemon: Arc<SharedCodexAppServer>) -> Self {
        Self(Arm::Codex(daemon, Arc::default()))
    }
}

/// What the Codex arm keeps for its one harness (#2348): the channel its thread's approval
/// requests go to, handed over at install, and its claim on the thread its turns run on.
#[derive(Default)]
struct CodexApprovals {
    held: std::sync::OnceLock<HeldRequestSender>,
    route: std::sync::Mutex<Option<ApprovalRoute>>,
}

impl CodexApprovals {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ApprovalRoute>> {
        self.route
            .lock()
            .expect("codex planner approval route mutex poisoned")
    }

    /// Claim `thread_id`'s approval requests anew for a turn, so a claim another harness took over
    /// in between is taken back. The earlier claim is given up after, which removes no later one.
    /// Before install there is nowhere to send them: they stay refused.
    fn route(&self, daemon: &SharedCodexAppServer, thread_id: &str) {
        let Some(held) = self.held.get() else {
            return;
        };
        let route = daemon.approval_routes().route(thread_id, held.clone());
        drop(self.lock().replace(route));
    }

    fn release(&self) {
        drop(self.lock().take());
    }
}

impl PlannerBackend {
    /// Query only the current worker's durable facts, including when no live backend exists.
    pub(crate) async fn run_recovery(
        provider: &AgentProvider,
        repo: &dyn Repo,
        runtime: &WorkerSessionProjection,
        live_phase: Option<HarnessPhaseTag>,
    ) -> Result<Option<RunRecovery>> {
        let phase = live_phase.or_else(|| {
            runtime
                .handle_state_json
                .clone()
                .and_then(HarnessSnapshot::parse_known)
                .map(|snapshot| snapshot.phase)
        });
        let unresolved = match provider {
            AgentProvider::OpenCode => {
                crate::acp_planner::recovery::submission_unresolved(repo, &runtime.id, live_phase)
                    .await?
            }
            AgentProvider::Codex | AgentProvider::Claude => false,
        };
        let cause = if unresolved {
            RunRecoveryCause::UnknownOutcome
        } else if phase == Some(HarnessPhaseTag::Wedged) {
            RunRecoveryCause::Wedged
        } else {
            return Ok(None);
        };
        Ok(Some(RunRecovery {
            cause,
            action: RunRecoveryAction::RestartSession,
        }))
    }

    /// Codex reports provider terminal confirmation directly. Local-process adapters record
    /// their confirmed terminal after teardown, before publishing the event; their event alone
    /// cannot prove teardown succeeded if that write failed.
    pub(crate) fn terminal_event_confirms_stop(&self) -> bool {
        matches!(&self.0, Arm::Codex(..))
    }

    /// Static support of the bound Planner backend, including before a session is opened.
    /// This is not admission: `compact_start` and the harness retain their current fences.
    pub(crate) fn manual_compact_supported(provider: &AgentProvider) -> bool {
        Self::metadata(provider).capabilities.manual_compact
    }

    /// Provider-owned suspension; the generic harness does not infer authentication policy.
    pub(crate) fn issuance_hold(&self) -> Option<String> {
        match &self.0 {
            Arm::Codex(daemon, _) => daemon.authentication_hold(),
            Arm::Claude(_) | Arm::Acp(_) => None,
        }
    }

    pub fn subscribe_events(&self) -> PlannerEvents {
        PlannerEvents(match &self.0 {
            Arm::Acp(session) => EventSource::Acp(session.subscribe_events()),
            Arm::Codex(daemon, _) => EventSource::Codex(CodexEvents::subscribe(daemon)),
            Arm::Claude(session) => EventSource::Claude(session.subscribe_events()),
        })
    }

    /// Resolve product transcript anchors in the server, before crossing the native seam.
    pub(crate) fn native_edit_target(
        &self,
        turn_id: &str,
        plan: &crate::harness::rewind::RewindPlan,
    ) -> Result<NativeEditTarget> {
        match &self.0 {
            Arm::Codex(..) | Arm::Acp(_) => Ok(NativeEditTarget::BeforeTurn {
                before_turn_id: turn_id.into(),
            }),
            Arm::Claude(_) => crate::claude_planner::rewind::native_edit_target(
                plan.previous_turn_id
                    .as_ref()
                    .map(|_| plan.previous_turn_rows.as_slice()),
                plan.prompt_client_id.as_deref(),
            ),
        }
    }

    /// Whether the deletion fence has sealed `thread_id`.
    pub fn thread_sealed(&self, thread_id: &str) -> bool {
        match &self.0 {
            Arm::Acp(session) => session.thread_sealed(thread_id),
            Arm::Codex(daemon, _) => daemon.thread_seals().is_sealed(thread_id),
            Arm::Claude(session) => session.thread_sealed(thread_id),
        }
    }

    /// Whether a queued entry can join the running turn (§5.9): the run loop checks this before
    /// it takes the entry out of the queue.
    pub fn supports_steer(&self) -> bool {
        match &self.0 {
            Arm::Codex(..) => true,
            Arm::Claude(_) | Arm::Acp(_) => false,
        }
    }

    /// Host selection preflight stays at the existing admission point, after snapshot persistence.
    pub(crate) async fn compact_request(
        &self,
        source: &impl SelectionSource,
    ) -> Result<CompactRequest> {
        match &self.0 {
            Arm::Claude(session) => session.compact_request(source).await,
            Arm::Codex(..) | Arm::Acp(_) => Ok(CompactRequest {
                selection: TurnModelSelection::inherit(),
            }),
        }
    }

    pub(crate) fn turn_identity(&self, id: String) -> TurnIdentity {
        match &self.0 {
            Arm::Codex(..) => TurnIdentity::Native(id),
            Arm::Claude(_) | Arm::Acp(_) => TurnIdentity::Local(CorrelationKey(id)),
        }
    }

    /// Local signal owners historically log/ignore failed delivery; only Codex propagated it.
    pub(crate) fn legacy_interrupt_result(
        &self,
        outcome: InterruptOutcome<InterruptDiagnostic>,
    ) -> std::result::Result<(), InterruptDiagnostic> {
        match &self.0 {
            Arm::Codex(..) => provider::codex::interrupt_result(outcome),
            Arm::Acp(_) | Arm::Claude(_) => match outcome {
                InterruptOutcome::NativeAck
                | InterruptOutcome::LocalSignalDelivered
                | InterruptOutcome::NoTarget
                | InterruptOutcome::Unconfirmed { .. } => Ok(()),
            },
        }
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        match &self.0 {
            Arm::Acp(session) => session.active_turn_id_for_thread(thread_id),
            Arm::Codex(daemon, _) => daemon.active_turn_id_for_thread(thread_id),
            Arm::Claude(session) => session.active_turn_id_for_thread(thread_id),
        }
    }

    pub(crate) async fn post_registry_shutdown_cleanup(&self, worker_session_id: &str) {
        match &self.0 {
            Arm::Claude(session) => {
                crate::claude_planner::lifecycle::shutdown_replay(session.host(), worker_session_id)
                    .await
            }
            Arm::Codex(..) | Arm::Acp(_) => {}
        }
    }

    pub fn provider(&self) -> AgentProvider {
        match &self.0 {
            Arm::Acp(session) => session.provider(),
            Arm::Codex(..) => AgentProvider::Codex,
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
            Arm::Codex(daemon, _) => {
                let payload = source.card_payload().await?;
                codex_selection::resolve(daemon, source, &payload).await
            }
            Arm::Claude(session) => session.resolve_selection(source).await,
        }
    }

    /// Release an idle harness's resources. Codex only detaches approvals here;
    /// the lifecycle owner unsubscribes its thread after removing the harness.
    /// This operation never sends a Codex turn interrupt.
    pub(crate) async fn hibernate(&self) -> Result<()> {
        match &self.0 {
            Arm::Codex(_, approvals) => {
                approvals.release();
                Ok(())
            }
            Arm::Acp(session) => session.shutdown().await,
            Arm::Claude(session) => session.shutdown().await,
        }
    }

    /// The registry installed the harness: a Claude or ACP session may start turns from now on,
    /// and the requests a Claude, Codex or ACP Planner's turns pause on go to `held`, the
    /// harness's channel (#2348).
    pub fn mark_installed(&self, held: &HeldRequestSender) {
        match &self.0 {
            Arm::Codex(_, approvals) => {
                let _ = approvals.held.set(held.clone());
            }
            Arm::Acp(session) => session.mark_installed(held.clone()),
            Arm::Claude(session) => session.mark_installed(held.clone()),
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
            Arm::Codex(..) | Arm::Acp(_) => None,
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

/// Which step of [`interrupt_codex_thread`] failed.
enum CodexInterruptStep<'a> {
    /// Interrupting the turn the daemon has cached for the thread.
    ActiveTurn,
    /// Interrupting the caller's known turn, tried only when the daemon had none cached.
    FallbackTurn(&'a str),
}

/// Codex's shutdown interrupt for one thread: the cached active turn, then `fallback_turn_id`
/// when the daemon had no turn cached before the first step. Each failure goes to `on_error`
/// as it happens and does not stop the next step.
async fn interrupt_codex_thread(
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    /// `RewindArm` as the binary before #2512 reads it, kept as that release's shape.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "provider", rename_all = "snake_case")]
    enum PreviousReader {
        Codex { before_turn_id: String },
        Claude { resume_at: Uuid, drops_turn: Uuid },
    }

    /// A rolled-back binary still reads a cut this one wrote, and an unapplied cut keeps its old shape.
    #[test]
    fn a_recorded_revert_stays_readable_by_the_previous_binary() {
        let unapplied = BackendRewind(RewindArm::Codex {
            before_turn_id: "turn-2".into(),
            reverted: false,
        });
        let applied = BackendRewind(RewindArm::Codex {
            before_turn_id: "turn-2".into(),
            reverted: true,
        });
        assert_eq!(
            serde_json::to_value(&unapplied).unwrap(),
            json!({"provider": "codex", "before_turn_id": "turn-2"})
        );
        let written = serde_json::to_value(&applied).unwrap();
        assert_eq!(
            serde_json::from_value::<PreviousReader>(written.clone()).unwrap(),
            PreviousReader::Codex {
                before_turn_id: "turn-2".into()
            }
        );
        assert_eq!(
            serde_json::from_value::<BackendRewind>(written).unwrap(),
            applied
        );
        assert_eq!(
            serde_json::from_value::<BackendRewind>(
                json!({"provider": "codex", "before_turn_id": "turn-2"})
            )
            .unwrap(),
            unapplied
        );
    }

    /// A sign-in hold that lands after `issuance_hold()` reaches `turn/start` as the daemon's refusal
    /// code: it awaits recovery like the hold itself, never the "change the model" refusal.
    #[test]
    fn the_sign_in_refusal_awaits_recovery_and_other_refusals_stay_refusals() {
        assert!(matches!(
            ordinary_start_result(codex_start_result(Err(
                crate::shared_codex_appserver::control::StartError::RecoveryRequired(
                    CalmError::CodexRefused(crate::codex_authentication::SIGN_IN_REFUSAL.into())
                )
            ))),
            Err(TurnStartFailure::AwaitingRecovery { .. })
        ));
        assert!(matches!(
            ordinary_start_result(codex_start_result(Err(
                crate::shared_codex_appserver::control::StartError::Refused(
                    CalmError::CodexRefused("turn/start failed: unknown model".into())
                )
            ))),
            Err(TurnStartFailure::Refused { .. })
        ));
    }
}
