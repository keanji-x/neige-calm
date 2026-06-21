//! MCP identity decision-write sink for the pre-PR7 MCP tool paths.
//!
//! This module is a structural seam for #679 PR6a-2. The production MCP
//! handlers derive their session-shaped persisted actor from
//! `ToolCallIdentity::to_actor_id()` and call the card-aware inherent methods
//! below. The principal-based
//! `DecisionSink::commit` entry remains inert until PR7 flips authority.

use crate::db::{RouteRepo, write_with_actor_events_typed};
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::model::{Card, Wave, WaveLifecycle};
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::recorder_shadow::{
    RecorderShadowDecisionKind, RecorderShadowDivergence, RecorderShadowProbe, emit_divergence,
};
use crate::session_projection_repo::RuntimeId;
use crate::state::WriteContext;
use crate::wave_lifecycle::{
    apply_requested_transition_in_tx, auto_promote_draft_in_tx, auto_transition_if_current_in_tx,
};
use crate::wave_report::{WaveReportPayload, persist_report_with_shadow};
use async_trait::async_trait;
use calm_exec::{AgentReactor, DecisionIntent, DecisionSink};
use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
use calm_types::error::CoreError;
use calm_types::observation::Observation;
use calm_types::worker::{Principal, WorkerSessionId};
use sqlx::{Sqlite, Transaction};
use std::sync::Arc;

#[derive(Clone)]
pub struct CardDecisionSink {
    repo: Arc<dyn RouteRepo>,
    events: EventBus,
    write: WriteContext,
}

impl CardDecisionSink {
    pub fn from_app_context(ctx: &Arc<AppContext>) -> Self {
        Self {
            repo: Arc::clone(&ctx.repo),
            events: ctx.events.clone(),
            write: ctx.write.clone(),
        }
    }

    pub async fn commit_worker_task_report(
        &self,
        identity: &ToolCallIdentity,
        event: Event,
    ) -> Result<(), CalmError> {
        let actor = identity.to_actor_id();
        let card_id_str = identity.card_id.clone();
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("emit: card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "emit: bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let wave = self
            .repo
            .wave_get(card.wave_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("emit: wave lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "emit: wave {} for card {} not found",
                    card.wave_id.as_str(),
                    card_id_str
                ))
            })?;

        let scope = EventScope::Card {
            card: CardId::from(card_id_str.clone()),
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_id = wave.id.clone();
        let release_workspace = matches!(
            event,
            Event::TaskCompleted { .. } | Event::TaskFailed { .. }
        );
        let worker_card_id_for_tx = card_id_str.clone();

        write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                let event = event.clone();
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_scope = wave_scope.clone();
                let wave_id = wave_id.clone();
                let worker_card_id = worker_card_id_for_tx.clone();
                Box::pin(async move {
                    // Issue #644 PR-B — flip the matching plan-task row INSIDE
                    // the same tx that persists the worker's report event
                    // (design §3): one tx, no event-persisted-but-row-stale
                    // crash window. The flips are guarded
                    // (`status IN ('dispatched','running')`, wave-pinned, and
                    // the done-flip skips gated rows), so a legacy
                    // `calm.task.dispatch` key with no tasks row, an already
                    // terminal row, or a foreign-wave id all no-op. This hook
                    // lives ONLY in the worker-role-gated
                    // `calm.task.complete` / `calm.task.fail` handlers — spec
                    // verdict emissions (`calm.task.verdict`, wave_state.rs)
                    // never run it, so verdicts can never flip rows.
                    let now = crate::model::now_ms();
                    let flip = match &event {
                        Event::TaskCompleted {
                            idempotency_key, ..
                        } => Some((idempotency_key.clone(), true)),
                        Event::TaskFailed {
                            idempotency_key, ..
                        } => Some((idempotency_key.clone(), false)),
                        _ => None,
                    };
                    // Issue #644 PR-C (§3): the `Working → Reviewing`
                    // auto-promotion is SUPPRESSED for a gated task's
                    // success report — the self-report is a claim, not
                    // evidence; the gate-result tx performs the promotion
                    // instead, on ANY gate verdict. Worker `task.failed`
                    // promotes as today (no gate runs on failure), and
                    // legacy keys with no tasks row keep today's behavior.
                    let mut suppress_promotion = false;
                    if let Some((task_id, success)) = flip {
                        // Round-4 review F1 — unstamped-row ownership proof:
                        // the REPORTING card must be the card the task's
                        // worker-spawn operation created (immutable op
                        // target, stamped in the same tx as the card). The
                        // card payload's `idempotency_key` is NOT proof —
                        // payloads are patchable via `PATCH /api/cards/{id}`,
                        // so a forged sibling payload could otherwise steal
                        // the report-beats-running-stamp window. For rows
                        // already stamped, the `worker_card_id = card` guard
                        // inside the flip implies the same binding.
                        let reporter = crate::db::sqlite::TaskReporter::Card {
                            card_id: worker_card_id.as_str(),
                            owns_key: crate::db::sqlite::worker_op_targets_card_tx(
                                tx,
                                &task_id,
                                &worker_card_id,
                            )
                            .await?,
                        };
                        let rows = if success {
                            match crate::db::sqlite::task_report_success_from_worker_tx(
                                tx,
                                &task_id,
                                wave_id.as_str(),
                                reporter,
                                now,
                            )
                            .await?
                            {
                                crate::db::sqlite::SuccessReportFlip::Done => 1,
                                crate::db::sqlite::SuccessReportFlip::Verifying => {
                                    // Gated row handed to the gate runner —
                                    // the gate-result tx promotes (§3).
                                    suppress_promotion = true;
                                    1
                                }
                                crate::db::sqlite::SuccessReportFlip::None => 0,
                            }
                        } else {
                            crate::db::sqlite::task_fail_from_worker_tx(
                                tx,
                                &task_id,
                                wave_id.as_str(),
                                reporter,
                                "worker-reported",
                                now,
                            )
                            .await?
                        };
                        // Round-2 review F3 — disambiguate a 0-row flip before
                        // emitting terminal side effects:
                        //   (i)   no tasks row for the key (legacy
                        //         `calm.task.dispatch` worker) → emit exactly
                        //         as before;
                        //   (iii) row already TERMINAL → duplicate/retried
                        //         report; keep emitting (consumers tolerate
                        //         duplicate task events per key, design §1.3 —
                        //         verdict emissions and report-retry
                        //         idempotency depend on it);
                        //   (iv)  row ACTIVE (`dispatched`/`running` — the
                        //         only states the guarded flip targets) and
                        //         the ownership guard rejected the reporter
                        //         → refuse the whole write: no event, no
                        //         Working → Reviewing transition; the
                        //         caller is told it does not own the task.
                        // Any other 0-row cause keeps today's emit behavior:
                        // (round-6 review) statuses the guarded UPDATE could
                        // never have matched — a legacy `calm.task.dispatch`
                        // key colliding with a still-`pending` plan row (or
                        // a `verifying` row whose gate is in flight) carries
                        // no ownership signal, so the legacy event must keep
                        // persisting with the row left untouched.
                        if rows == 0
                            && let Some(row) = crate::db::sqlite::task_get_tx(tx, &task_id).await?
                        {
                            // Issue #644 PR-C (§3): a duplicate / retried
                            // success report for a GATED row (already
                            // `verifying`, or already terminal via a gate
                            // verdict) must not promote — exactly one
                            // promotion per gated task, in the gate-result
                            // tx.
                            if success && row.gate_json.is_some() {
                                suppress_promotion = true;
                            }
                            if matches!(
                                row.status,
                                crate::model::TaskStatus::Dispatched
                                    | crate::model::TaskStatus::Running
                            ) {
                                let owns = match &row.worker_card_id {
                                    Some(owner) => *owner == worker_card_id,
                                    None => matches!(
                                        reporter,
                                        crate::db::sqlite::TaskReporter::Card {
                                            owns_key: true,
                                            ..
                                        }
                                    ),
                                };
                                if !owns {
                                    return Err(CalmError::Forbidden(format!(
                                        "task {task_id} is not owned by reporting card \
                                         {worker_card_id}; report rejected"
                                    )));
                                }
                            }
                        }
                    }

                    let mut events = vec![(actor, scope, event)];
                    if !suppress_promotion
                        && let Some(auto_events) = auto_transition_if_current_in_tx(
                            tx,
                            &wave_id,
                            crate::model::WaveLifecycle::Working,
                            crate::model::WaveLifecycle::Reviewing,
                            &ActorId::Kernel,
                            Some("[auto] first task report".to_string()),
                        )
                        .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                        );
                    }
                    Ok(((), events))
                })
            },
        )
        .await?;

        if release_workspace {
            // Normal worker reports release only the lease row; downstream PR
            // flow still needs the worker worktree and slice branch.
            release_workspace_lease_for_card_repo(self.repo.as_ref(), &self.events, &card_id_str)
                .await?;
        }

        Ok(())
    }

    pub async fn commit_spec_verdict(
        &self,
        identity: &ToolCallIdentity,
        message: String,
        lifecycle: Option<WaveLifecycle>,
        event: Event,
    ) -> Result<(), CalmError> {
        let actor = identity.to_actor_id();
        let card_id_str = identity.card_id.clone();
        let principal = identity.to_principal();
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("wave_state: card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "wave_state: bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let wave = self
            .repo
            .wave_get(card.wave_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("wave_state: wave lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "wave_state: wave {} for card {} not found",
                    card.wave_id.as_str(),
                    card_id_str
                ))
            })?;
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_id = wave.id.clone();
        let wave_scope = scope.clone();
        let recorder_shadow = Arc::new(CardDecisionSinkRecorderShadowProbe {
            principal,
            wave_id: wave_id.clone(),
        });

        write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                let event = event.clone();
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_scope = wave_scope.clone();
                let wave_id = wave_id.clone();
                let message = message.clone();
                let recorder_shadow = Arc::clone(&recorder_shadow);
                Box::pin(async move {
                    let mut events = Vec::new();
                    if let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id).await? {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                        );
                    }
                    if let Some(target) = lifecycle
                        && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                            tx,
                            &wave_id,
                            target,
                            &actor,
                            message.clone(),
                        )
                        .await?
                    {
                        recorder_shadow
                            .record(tx, RecorderShadowDecisionKind::WaveLifecycle)
                            .await?;
                        events.extend(
                            lifecycle_events
                                .into_iter()
                                .map(|event| (actor.clone(), wave_scope.clone(), event)),
                        );
                    }
                    events.push((actor, scope, event));
                    Ok(((), events))
                })
            },
        )
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn commit_report_write(
        &self,
        identity: &ToolCallIdentity,
        wave: Wave,
        report_card: Card,
        current_payload: WaveReportPayload,
        next: WaveReportPayload,
        agent_message: String,
        lifecycle: Option<WaveLifecycle>,
    ) -> Result<Card, CalmError> {
        let actor = identity.to_actor_id();
        let principal = identity.to_principal();
        let recorder_shadow: Arc<dyn RecorderShadowProbe> =
            Arc::new(CardDecisionSinkRecorderShadowProbe {
                principal,
                wave_id: wave.id.clone(),
            });
        let updated = persist_report_with_shadow(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            actor,
            EditAuthor::Spec,
            wave,
            report_card,
            current_payload,
            next,
            Some(agent_message),
            lifecycle,
            true,
            Some(recorder_shadow),
        )
        .await?;
        Ok(updated)
    }
}

struct CardDecisionSinkRecorderShadowProbe {
    principal: Option<Principal>,
    wave_id: WaveId,
}

#[async_trait]
impl RecorderShadowProbe for CardDecisionSinkRecorderShadowProbe {
    async fn record(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        decision_kind: RecorderShadowDecisionKind,
    ) -> Result<(), CalmError> {
        let principal = self.principal.as_ref().ok_or_else(|| {
            CalmError::Forbidden("recorder gate requires an agent principal".into())
        })?;
        let Principal::Agent { session_id, .. } = principal else {
            return Err(CalmError::Forbidden(
                "recorder gate requires an agent session".into(),
            ));
        };
        match PrincipalDecisionGate::new(principal.clone())
            .decide_recorder(tx, &self.wave_id)
            .await
        {
            Ok(GateDecision::Allow) => Ok(()),
            Ok(GateDecision::Deny(message)) => {
                emit_divergence(&RecorderShadowDivergence {
                    wave_id: self.wave_id.clone(),
                    session_id: session_id.clone(),
                    decision_kind,
                });
                Err(CalmError::Forbidden(format!(
                    "recorder gate denied {}: {message}",
                    decision_kind.as_str()
                )))
            }
            Err(error) => {
                tracing::warn!(
                    target: "neige::recorder_shadow",
                    wave_id = %self.wave_id,
                    session_id = %session_id,
                    decision_kind = decision_kind.as_str(),
                    error = %error,
                    "recorder gate computation failed; denying card-era write"
                );
                Err(CalmError::Forbidden(format!(
                    "recorder gate failed closed for {}: {error}",
                    decision_kind.as_str()
                )))
            }
        }
    }
}

#[async_trait]
impl DecisionSink for CardDecisionSink {
    async fn commit(
        &self,
        _principal: &Principal,
        _intent: DecisionIntent,
    ) -> Result<(), CoreError> {
        Err(CoreError::Internal(
            "CardDecisionSink production path uses the card-aware methods; principal commit lands with PR7"
                .into(),
        ))
    }
}

/// Structural-only spec-harness reactor for #679 PR6a-2.
///
/// No `worker_sessions` row backs a spec harness yet, and nothing reads this
/// principal until PR7. The live run loop still delivers observations through
/// the existing turn queue and is deliberately not wired to this stub.
#[derive(Clone, Debug)]
pub struct SpecHarnessAgentReactor {
    runtime_id: RuntimeId,
    wave_id: WaveId,
    cove_id: CoveId,
}

impl SpecHarnessAgentReactor {
    pub fn new(runtime_id: RuntimeId, wave_id: WaveId, cove_id: CoveId) -> Self {
        Self {
            runtime_id,
            wave_id,
            cove_id,
        }
    }
}

#[async_trait]
impl AgentReactor for SpecHarnessAgentReactor {
    fn principal(&self) -> Principal {
        Principal::Agent {
            session_id: WorkerSessionId::from(self.runtime_id.clone()),
            wave_id: self.wave_id.clone(),
            cove_id: self.cove_id.clone(),
        }
    }

    async fn react(&self, _observation: &Observation) -> Result<Vec<DecisionIntent>, CoreError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{
        SqlxRepo, begin_immediate_tx, session_insert_tx, session_mark_wave_root_tx,
    };
    use crate::model::{CardRole, NewCard, NewCove, NewWave, WavePatch};
    use crate::operation::workspace_lease::{
        acquire_workspace_lease_tx, prepare_workspace_lease_target_tx, provision_workspace_worktree,
    };
    use crate::recorder_shadow::divergence_count_for_test;
    use crate::wave_cove_cache::WaveCoveCache;
    use calm_types::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
        WorkerSessionState,
    };
    use serde_json::Value;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::Context as TracingContext;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{Layer, registry as tracing_registry};

    struct RecorderShadowWarnLayer {
        hits: Arc<AtomicUsize>,
    }

    impl<S> Layer<S> for RecorderShadowWarnLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: TracingContext<'_, S>) {
            if event.metadata().target() == "neige::recorder_shadow"
                && *event.metadata().level() == tracing::Level::WARN
            {
                self.hits.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn worker_session(id: &str, wave_id: WaveId, card_id: CardId) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from(id),
            wave_id,
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Planner,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Starting,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(card_id),
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    async fn seed_wave_root_session(
        repo: &SqlxRepo,
        wave_id: &WaveId,
        card_id: &CardId,
        session_id: &WorkerSessionId,
    ) {
        let root_session = worker_session(session_id.as_str(), wave_id.clone(), card_id.clone());
        let wave_id = wave_id.clone();
        let session_id = session_id.clone();
        crate::db::write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                session_insert_tx(tx, root_session)
                    .await
                    .map_err(CalmError::from)?;
                session_mark_wave_root_tx(tx, &wave_id, &session_id)
                    .await
                    .map_err(CalmError::from)?;
                Ok(())
            })
        })
        .await
        .expect("seed wave root session");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_task_report_releases_lease_but_preserves_git_worktree_branch() {
        let repo_root = tempfile::tempdir().expect("repo tempdir");
        init_git_repo(repo_root.path());
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "worker report preserve".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "worker report preserve".into(),
                sort: None,
                cwd: repo_root.path().display().to_string(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let worker_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .expect("create worker card");
        let session_id = WorkerSessionId::from("worker-session");
        let session = worker_session(session_id.as_str(), wave.id.clone(), worker_card.id.clone());
        crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                session_insert_tx(tx, session)
                    .await
                    .map_err(CalmError::from)?;
                Ok(())
            })
        })
        .await
        .expect("seed worker session");

        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        let target =
            prepare_workspace_lease_target_tx(&mut tx, wave.id.as_str(), worker_card.id.as_str())
                .await
                .expect("prepare lease target");
        let (lease, _event) = acquire_workspace_lease_tx(
            &mut tx,
            worker_card.id.as_str(),
            wave.id.as_str(),
            "op-worker-report-preserve",
            &target,
        )
        .await
        .expect("acquire lease");
        tx.commit().await.expect("commit lease");
        provision_workspace_worktree(&target).expect("provision worktree");
        std::fs::write(target.path.join("worker-output.txt"), "worker commit\n")
            .expect("write worker output");
        run_git(&target.path, ["add", "worker-output.txt"]);
        run_git(&target.path, ["commit", "-m", "worker output"]);

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wave_cove_cache)
            .await
            .expect("seed wave cove cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, wave_cove_cache),
        };
        let identity = ToolCallIdentity {
            card_id: worker_card.id.as_str().to_string(),
            role: CardRole::Worker,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: session_id.as_str().to_string(),
            wave_id: Some(wave.id.as_str().to_string()),
            cove_id: cove.id.as_str().to_string(),
            thread_id: "worker-thread".to_string(),
        };

        sink.commit_worker_task_report(
            &identity,
            Event::TaskCompleted {
                idempotency_key: "worker-report-preserve".into(),
                result: Value::Null,
                artifacts: Vec::new(),
                agent_message: None,
            },
        )
        .await
        .expect("commit worker report");

        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
                .bind(&lease.lease_id)
                .fetch_one(repo.pool())
                .await
                .expect("lease state");
        assert_eq!(state, "released");
        assert!(
            target.path.is_dir(),
            "DecisionSink task completion preserves worker worktree"
        );
        assert!(
            git_ref_exists(repo_root.path(), &format!("refs/heads/{}", target.branch)),
            "DecisionSink task completion preserves slice branch"
        );
        let removed_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
                .fetch_one(repo.pool())
                .await
                .expect("removed event count");
        assert_eq!(removed_events, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_root_report_write_is_forbidden_under_recorder_enforce() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "recorder-shadow".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "shadow wave".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let spec_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .expect("create spec card");
        let report_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())
                    .expect("initial report payload"),
            })
            .await
            .expect("create report card");

        let root_session_id = WorkerSessionId::from("root-session");
        seed_wave_root_session(repo.as_ref(), &wave.id, &spec_card.id, &root_session_id).await;

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
        card_role_cache.insert(
            report_card.id.clone(),
            CardRole::ReportCard,
            wave.id.clone(),
        );
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wave_cove_cache)
            .await
            .expect("seed wave cove cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, wave_cove_cache),
        };
        let identity = ToolCallIdentity {
            card_id: spec_card.id.as_str().to_string(),
            role: CardRole::Spec,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: "non-root-session".to_string(),
            wave_id: Some(wave.id.as_str().to_string()),
            cove_id: cove.id.as_str().to_string(),
            thread_id: "non-root-thread".to_string(),
        };
        let next = WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            summary: "non-root summary".into(),
            body: "# Goal\n\nnon-root body\n".into(),
        };
        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_registry().with(RecorderShadowWarnLayer {
            hits: Arc::clone(&warnings),
        });
        let _guard = tracing::subscriber::set_default(subscriber);
        let before_divergences = divergence_count_for_test();
        let before_events = repo
            .events_since(0, None)
            .await
            .expect("events before commit")
            .len();
        let before_report = repo
            .card_get(report_card.id.as_str())
            .await
            .expect("report before")
            .expect("report row");

        let err = sink
            .commit_report_write(
                &identity,
                wave.clone(),
                report_card,
                WaveReportPayload::initial(),
                next,
                "non-root edit".into(),
                None,
            )
            .await
            .expect_err("non-root report write must be forbidden");

        assert!(
            matches!(err, CalmError::Forbidden(ref message) if message.contains("not wave root"))
        );
        assert_eq!(divergence_count_for_test(), before_divergences + 1);
        assert_eq!(warnings.load(Ordering::Relaxed), 1);

        let after_report = repo
            .card_get(before_report.id.as_str())
            .await
            .expect("report after")
            .expect("report row");
        assert_eq!(after_report.payload, before_report.payload);
        let events = repo.events_since(0, None).await.expect("events");
        assert_eq!(events.len(), before_events);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_report_write_with_lifecycle_succeeds_under_recorder_enforce() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "recorder-root".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "root wave".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let wave = repo
            .wave_update(
                wave.id.as_str(),
                WavePatch {
                    lifecycle: Some(WaveLifecycle::Planning),
                    ..Default::default()
                },
            )
            .await
            .expect("set planning");
        let spec_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .expect("create spec card");
        let report_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())
                    .expect("initial report payload"),
            })
            .await
            .expect("create report card");

        let root_session_id = WorkerSessionId::from("root-session");
        seed_wave_root_session(repo.as_ref(), &wave.id, &spec_card.id, &root_session_id).await;

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
        card_role_cache.insert(
            report_card.id.clone(),
            CardRole::ReportCard,
            wave.id.clone(),
        );
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wave_cove_cache)
            .await
            .expect("seed wave cove cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, wave_cove_cache),
        };
        let identity = ToolCallIdentity {
            card_id: spec_card.id.as_str().to_string(),
            role: CardRole::Spec,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: root_session_id.as_str().to_string(),
            wave_id: Some(wave.id.as_str().to_string()),
            cove_id: cove.id.as_str().to_string(),
            thread_id: "root-thread".to_string(),
        };
        let next = WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            summary: "root summary".into(),
            body: "# Goal\n\nroot body\n".into(),
        };

        let updated = sink
            .commit_report_write(
                &identity,
                wave.clone(),
                report_card,
                WaveReportPayload::initial(),
                next,
                "root edit".into(),
                Some(WaveLifecycle::Dispatching),
            )
            .await
            .expect("root report write succeeds");

        let payload: WaveReportPayload =
            serde_json::from_value(updated.payload).expect("updated report payload");
        assert_eq!(payload.summary, "root summary");
        let wave_after = repo
            .wave_get(wave.id.as_str())
            .await
            .expect("wave after")
            .expect("wave row");
        assert_eq!(wave_after.lifecycle, WaveLifecycle::Dispatching);
        let events = repo.events_since(0, None).await.expect("events");
        assert!(events.iter().any(|(_, _, _, event)| matches!(
            event,
            Event::WaveLifecycleChanged {
                to: WaveLifecycle::Dispatching,
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|(_, _, _, event)| matches!(event, Event::WaveReportEdited { .. }))
        );
    }

    #[tokio::test]
    async fn spec_harness_agent_reactor_is_inert_and_shapes_principal() {
        let reactor = SpecHarnessAgentReactor::new(
            "runtime-1".to_string(),
            WaveId::from("wave-1"),
            CoveId::from("cove-1"),
        );

        assert_eq!(
            reactor.principal(),
            Principal::Agent {
                session_id: WorkerSessionId::from("runtime-1"),
                wave_id: WaveId::from("wave-1"),
                cove_id: CoveId::from("cove-1"),
            }
        );

        let intents = reactor
            .react(&Observation::WaveGoal {
                text: "goal".into(),
            })
            .await
            .expect("react succeeds");
        assert!(intents.is_empty());
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("create git repo dir");
        run_git(path, ["init"]);
        run_git(path, ["config", "user.email", "sink@example.test"]);
        run_git(path, ["config", "user.name", "Sink Test"]);
        std::fs::write(path.join("README.md"), "initial\n").expect("write readme");
        run_git(path, ["add", "README.md"]);
        run_git(path, ["commit", "-m", "initial"]);
    }

    fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args,
            repo.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_ref_exists(repo: &Path, full_ref: &str) -> bool {
        Command::new("git")
            .args(["show-ref", "--verify", "--quiet", full_ref])
            .current_dir(repo)
            .status()
            .expect("spawn git show-ref")
            .success()
    }
}
