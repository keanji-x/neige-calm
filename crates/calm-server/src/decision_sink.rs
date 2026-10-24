//! MCP identity decision-write sink for the MCP tool paths.

mod planner_message;
mod worker_report;
pub(crate) use worker_report::admit_worker_write_tx;

pub use worker_report::WorkerTaskReport;

use crate::db::{RouteRepo, write_with_actor_events_typed};
use crate::error::CalmError;
#[cfg(test)]
use crate::event::EditAuthor;
use crate::event::{Event, EventBus, EventScope};
pub use crate::git_candidate::commit_message::{CommitMessage, DeliveryMessage};
use crate::ids::CardId;
#[cfg(test)]
use crate::ids::TrackId;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::model::{Card, CardRole};
use crate::operation::workspace_lease::release_workspace_lease_for_card_tx;
use crate::report_sources::{self, SourceLinkWarning};
use crate::state::WriteContext;
#[cfg(test)]
use crate::track_report::TrackReportPayload;
use crate::track_report::{self, BlockOpOutcome, ReportBlock, ReportDocOp};
use std::sync::Arc;

#[derive(Clone)]
pub struct CardDecisionSink {
    repo: Arc<dyn RouteRepo>,
    events: EventBus,
    write: WriteContext,
    /// Read-only, for the post-commit `report_sources` lookup behind the receipt warnings; `None` means no warnings are computed.
    sqlite_db: Option<crate::db::sqlite::SqliteDb>,
}

/// The updated card, the block-level outcome where the op has one, and the unresolved `neige://source/` links of the prose the write touched.
#[derive(Debug, Clone)]
pub struct ReportOpCommit {
    pub card: Card,
    pub block: Option<BlockOpOutcome>,
    pub warnings: Vec<SourceLinkWarning>,
    pub anchor: serde_json::Value,
}

impl CardDecisionSink {
    pub fn from_app_context(ctx: &Arc<AppContext>) -> Self {
        Self {
            repo: Arc::clone(&ctx.repo),
            events: ctx.events.clone(),
            write: ctx.write.clone(),
            sqlite_db: ctx.sqlite_db.clone(),
        }
    }

    pub async fn commit_worker_task_report(
        &self,
        identity: &ToolCallIdentity,
        report: WorkerTaskReport,
    ) -> Result<(), CalmError> {
        let identity_for_tx = identity.clone();
        let actor = identity.to_actor_id();
        let card_id_str = identity.card_id.clone();
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let track = self
            .repo
            .track_get(card.track_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("track lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "track {} for card {} not found",
                    card.track_id.as_str(),
                    card_id_str
                ))
            })?;

        let scope = EventScope::Card {
            card: CardId::from(card_id_str.clone()),
            track: track.id.clone(),
            area: track.area_id.clone(),
        };
        let track_id = track.id.clone();
        let worker_card_id_for_tx = card_id_str.clone();

        let write_result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                let identity = identity_for_tx.clone();
                let report = report.clone();
                let actor = actor.clone();
                let scope = scope.clone();
                let track_id = track_id.clone();
                let worker_card_id = worker_card_id_for_tx.clone();
                Box::pin(async move {
                    admit_worker_write_tx(tx, &identity, &track_id).await?;
                    // Admission, task CAS and report event share one transaction; same-outcome repeats roll back as idempotent success.
                    let now = crate::model::now_ms();
                    let task_id = report.attempt_id().to_string();
                    // The failure branch keeps the worker's own `reason` beside the `worker-reported` classifier.
                    let failure_reason = match &report {
                        WorkerTaskReport::Completed { .. } => None,
                        WorkerTaskReport::Failed { reason, .. } => Some(reason.clone()),
                    };
                    let success = failure_reason.is_none();
                    // Unstamped-row ownership proof: the REPORTING card must be the card the task's worker-spawn operation created. The card payload's `idempotency_key` is NOT proof — payloads are patchable via `PATCH /api/cards/{id}`.
                    let reporter = crate::db::sqlite::TaskReporter::Card {
                        card_id: worker_card_id.as_str(),
                        owns_key: crate::db::sqlite::worker_op_targets_card_tx(
                            tx,
                            &task_id,
                            &worker_card_id,
                        )
                        .await?,
                    };
                    if worker_report::admit_worker_report_tx(
                        tx,
                        &task_id,
                        track_id.as_str(),
                        reporter,
                        success,
                    )
                    .await?
                    {
                        return Err(CalmError::Conflict(worker_report::REPEATED.into()));
                    }
                    if success {
                        let unread = crate::db::sqlite::planner_messages_unread_tx(tx, &task_id).await?;
                        if !unread.is_empty() {
                            return Err(CalmError::Conflict(format!("unread Planner messages: {}; read them with neige_message_read before reporting", unread.iter().map(|m| m.message_id.as_str()).collect::<Vec<_>>().join(", "))));
                        }
                    }
                    worker_report::check_result_contract_tx(tx, &task_id, &report).await?;
                    let delivery = report.release_delivery();
                    let mut notification = calm_types::event::PlannerNotification::Notify;
                    let rows = if success {
                        match crate::db::sqlite::task_report_success_from_worker_tx(
                            tx,
                            &task_id,
                            track_id.as_str(),
                            reporter,
                            now,
                        )
                        .await?
                        {
                            crate::db::sqlite::SuccessReportFlip::Done => 1,
                            // Gated row handed to the gate runner.
                            crate::db::sqlite::SuccessReportFlip::Verifying => { notification = calm_types::event::PlannerNotification::Suppress; 1 },
                            crate::db::sqlite::SuccessReportFlip::None => 0,
                        }
                    } else {
                        crate::db::sqlite::task_fail_from_worker_tx(
                            tx,
                            &task_id,
                            track_id.as_str(),
                            reporter,
                            &crate::db::sqlite::status_detail_with_reason(
                                "worker-reported",
                                failure_reason.as_deref().unwrap_or_default(),
                            ),
                            now,
                        )
                        .await?
                    };
                    if rows == 0
                        && crate::db::sqlite::task_get_tx(tx, &task_id)
                            .await?
                            .is_some()
                    {
                        return Err(CalmError::Conflict(format!(
                            "task {task_id}: admitted report did not advance the task"
                        )));
                    }
                    if let WorkerTaskReport::Completed { result, .. } = &report
                        && let Some(task) = crate::db::sqlite::task_get_tx(tx, &task_id).await? {
                        calm_truth::objective_host::check_jobs::report_done_tx(tx, &track_id, &task.key, result).await?;
                    }
                    // #1830 S2 D7: the lease is released in this transaction, and the first delivery row
                    // (commit the track's checkout as this attempt ended, with the worker's message on a
                    // completion that carries one) lands with it when the card's lease is a
                    // kernel-delivery lease. A REPEATED report never reaches here, so a second report
                    // never writes a second row; a crash can no longer leave the lease `held`.
                    let released = release_workspace_lease_for_card_tx(
                        tx,
                        &worker_card_id,
                        delivery,
                    )
                    .await?;

                    if success && crate::git_candidate::delivery::delivery_latest_for_attempt_tx(tx, &task_id).await?.is_some() {
                        notification = calm_types::event::PlannerNotification::Suppress;
                    }
                    let mut events = vec![(actor, scope, report.event(notification))];
                    events.extend(released);
                    Ok(((), events))
                })
            },
        )
        .await;
        match write_result {
            Ok(_) => {}
            Err(CalmError::Conflict(reason)) if reason == worker_report::REPEATED => {}
            Err(error) => return Err(error),
        }

        Ok(())
    }

    pub async fn commit_planner_verdict(
        &self,
        identity: &ToolCallIdentity,
        event: Event,
    ) -> Result<(), CalmError> {
        if identity.role != CardRole::Planner {
            return Err(CalmError::Forbidden(
                "candidate verdict requires Planner identity".into(),
            ));
        }
        let actor = identity.to_actor_id();
        let card_id_str = identity.card_id.clone();
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let track = self
            .repo
            .track_get(card.track_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("track lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "track {} for card {} not found",
                    card.track_id.as_str(),
                    card_id_str
                ))
            })?;
        let attempt_id = match &event {
            Event::TaskCompleted {
                idempotency_key, ..
            }
            | Event::TaskFailed {
                idempotency_key, ..
            } => idempotency_key.clone(),
            _ => {
                return Err(CalmError::Internal(
                    "a planner verdict lowers only to a task outcome".into(),
                ));
            }
        };
        let scope = EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        };

        let committed = crate::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                // A verdict names one task execution of the caller's track; the runs projection would drop any other key silently.
                let owned = crate::db::sqlite::task_get_tx(tx, &attempt_id)
                    .await?
                    .is_some_and(|task| task.track_id == track.id.as_str());
                if !owned {
                    return Err(CalmError::NotFound(format!(
                        "attempt_id {attempt_id} is not a task attempt of this track; verdict refused"
                    )));
                }
                let id = crate::db::sqlite::append_decision_event_in_tx(
                    tx, &actor, &scope, None, &event,
                )
                .await?;
                Ok(vec![crate::event::BroadcastEnvelope {
                    id,
                    event_version: crate::event::SYNC_EVENT_VERSION,
                    actor,
                    scope,
                    event,
                }])
            })
        })
        .await?;
        for event in committed {
            self.events.emit_envelope(event);
        }
        Ok(())
    }

    /// The agent-MCP report write: the recorder admission gate and the persist boundary, with an arbitrary [`ReportDocOp`] executed inside the transaction.
    /// The single funnel every block-channel write passes through, so attribution is decided here, once, from `identity.role`: hard-coding `Planner` would attribute an assistant's edits to the planner.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_report_op(
        &self,
        identity: &ToolCallIdentity,
        op: ReportDocOp,
        agent_message: Option<String>,
    ) -> Result<ReportOpCommit, CalmError> {
        let (card, trace) = track_report::write::agent_report_intent(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            identity.clone(),
            op,
            agent_message,
        )
        .await?;
        let track_id = card.track_id.clone();
        let current_rev = card
            .payload
            .get("docRev")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| CalmError::Internal("written Report lacks docRev".into()))?;
        let anchor = crate::report_read_ledger::anchor(trace.knowledge.as_ref(), current_rev);
        // After the transaction committed, before the receipt is assembled. Never blocks the write; a lookup failure here is the receipt's problem, not the row's.
        let warnings = match self.sqlite_db.as_ref() {
            Some(pool) if !trace.written_prose_block_ids.is_empty() => {
                let blocks = card
                    .payload
                    .get("blocks")
                    .cloned()
                    .map(serde_json::from_value::<Vec<ReportBlock>>)
                    .transpose()
                    .map_err(|e| {
                        CalmError::Internal(format!("track_report: decode written blocks: {e}"))
                    })?
                    .unwrap_or_default();
                report_sources::warnings::for_write(
                    pool.read_pool(),
                    track_id.as_str(),
                    &blocks,
                    &trace.written_prose_block_ids,
                )
                .await?
            }
            _ => Vec::new(),
        };
        Ok(ReportOpCommit {
            card,
            block: trace.block,
            warnings,
            anchor,
        })
    }
}

#[cfg(test)]
use crate::track_report::identity::report_op_attribution;

pub(crate) use crate::recorder_admission::CardDecisionSinkRecorderAdmissionGate;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, begin_immediate_tx, session_start_runtime_tx};
    use crate::model::{CardRole, NewArea, NewCard, NewTrack};
    use crate::operation::workspace_lease::{
        acquire_workspace_lease_tx, prepare_worker_lease_as_tx,
    };
    use crate::track_area_cache::TrackAreaCache;
    use calm_truth::db::RepoEventWrite as _;
    use calm_truth::db::RepoRead;
    use calm_types::worker::WorkerSessionId;
    use calm_types::worker::WorkerSessionState;
    use serde_json::{Value, json};
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::Context as TracingContext;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{Layer, registry as tracing_registry};

    /// The funnel's role table, pinned at the one layer where every role is reachable (the report tools' declared `roles`, which the registry gates, mask Worker/ReportCard today).
    #[test]
    fn report_op_attribution_refuses_worker_and_report_cards() {
        assert_eq!(
            report_op_attribution(CardRole::Planner).expect("planner writes its own report"),
            EditAuthor::Planner
        );
        assert_eq!(
            report_op_attribution(CardRole::Assistant).expect("assistant writes the report"),
            EditAuthor::Assistant
        );
        for role in [CardRole::Worker, CardRole::ReportCard] {
            match report_op_attribution(role) {
                Err(CalmError::Forbidden(msg)) => assert!(
                    msg.contains("may not write the track report"),
                    "{role:?} refusal should say why, got {msg:?}"
                ),
                other => panic!(
                    "{role:?} must be refused outright, not attributed to the planner; got {other:?}"
                ),
            }
        }
    }

    struct RecorderAdmissionWarnLayer {
        hits: Arc<AtomicUsize>,
    }

    impl<S> Layer<S> for RecorderAdmissionWarnLayer
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

    async fn seed_track_root_session(
        repo: &SqlxRepo,
        track_id: &TrackId,
        card_id: &CardId,
        session_id: &WorkerSessionId,
    ) {
        let init = crate::session_projection_repo::WorkerSessionInit::shared_planner(
            session_id.to_string(),
            card_id.to_string(),
            crate::session_projection_repo::AgentProvider::Codex,
            WorkerSessionState::Starting,
            None,
            json!({}),
            1,
        );
        assert_eq!(
            repo.card_get(card_id.as_str())
                .await
                .unwrap()
                .unwrap()
                .track_id,
            *track_id
        );
        crate::db::write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                session_start_runtime_tx(tx, init).await?;
                Ok(())
            })
        })
        .await
        .expect("seed track root session");
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
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "worker report preserve".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .expect("create area");
        let track = crate::test_seams::create_track_with_workspace_for_test(
            repo.db(),
            NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "worker report preserve".into(),
                sort: None,
                cwd: repo_root.path().display().to_string(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            },
            &crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(
                repo_root.path().into(),
            ),
            &crate::track_area_cache::TrackAreaCache::new(),
        )
        .await
        .expect("create track");
        let worker_card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .expect("create worker card");
        let session_id = WorkerSessionId::from("worker-session");
        let init = crate::session_projection_repo::WorkerSessionInit {
            id: session_id.to_string(),
            card_id: worker_card.id.to_string(),
            kind: crate::session_projection_repo::WorkerSessionKind::CodexCard,
            agent_provider: None,
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 1,
        };
        crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                session_start_runtime_tx(tx, init).await?;
                Ok(())
            })
        })
        .await
        .expect("seed worker session");

        let worktree =
            crate::test_support::ensure_track_worktree(repo.db(), track.id.as_str()).await;
        let mut tx = begin_immediate_tx(repo.db()).await.expect("begin tx");
        let plan = prepare_worker_lease_as_tx(
            &mut tx,
            track.id.as_str(),
            crate::model::TaskAccess::ReadWrite,
            &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        )
        .await
        .expect("prepare the worker lease");
        let (lease, _event) = acquire_workspace_lease_tx(
            &mut tx,
            worker_card.id.as_str(),
            track.id.as_str(),
            "op-worker-report-preserve",
            &plan,
        )
        .await
        .expect("acquire lease");
        tx.commit().await.expect("commit lease");
        assert_eq!(plan.path, worktree);
        std::fs::write(plan.path.join("worker-output.txt"), "worker commit\n")
            .expect("write worker output");
        run_git(&plan.path, ["add", "worker-output.txt"]);
        run_git(&plan.path, ["commit", "-m", "worker output"]);

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());
        let track_area_cache = TrackAreaCache::new();
        repo.seed_track_area_cache(&track_area_cache)
            .await
            .expect("seed track area cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, track_area_cache),
            sqlite_db: repo.sqlite_db(),
        };
        let identity = ToolCallIdentity {
            card_id: worker_card.id.as_str().to_string(),
            role: CardRole::Worker,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: session_id.as_str().to_string(),
            track_id: Some(track.id.as_str().to_string()),
            area_id: area.id.as_str().to_string(),
            thread_id: "worker-thread".to_string(),
        };

        sink.commit_worker_task_report(
            &identity,
            WorkerTaskReport::Completed {
                attempt_id: "worker-report-preserve".into(),
                result: Value::Null,
                artifacts: Vec::new(),
                commit_message: Some(CommitMessage::parse("worker output").unwrap()),
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
            plan.path.is_dir(),
            "DecisionSink task completion preserves the track worktree"
        );
        assert!(
            git_ref_exists(repo_root.path(), &format!("refs/heads/{}", plan.branch)),
            "DecisionSink task completion preserves the track branch"
        );
        let removed_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
                .fetch_one(repo.pool())
                .await
                .expect("removed event count");
        assert_eq!(removed_events, 0);
    }

    /// An identity whose `session_id` has no `worker_sessions` row is a denial. Pins the fail-closed shape: `Forbidden`, one warning, and the report row and event log both untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn report_write_from_an_unresolvable_session_is_forbidden_under_recorder_enforce() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "recorder-admission".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .expect("create area");
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "admission track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create track");
        let planner_card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"planner_provider":"codex"}),
            })
            .await
            .expect("create planner card");
        let report_card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial())
                    .expect("initial report payload"),
            })
            .await
            .expect("create report card");

        let root_session_id = WorkerSessionId::from("root-session");
        seed_track_root_session(repo.as_ref(), &track.id, &planner_card.id, &root_session_id).await;
        // The recorder gate reads `cards.role` in-tx; `card_create` persists `worker` regardless of kind.
        sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
            .bind(planner_card.id.as_str())
            .execute(repo.pool())
            .await
            .expect("persist planner card role");

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
        card_role_cache.insert(
            report_card.id.clone(),
            CardRole::ReportCard,
            track.id.clone(),
        );
        let track_area_cache = TrackAreaCache::new();
        repo.seed_track_area_cache(&track_area_cache)
            .await
            .expect("seed track area cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, track_area_cache),
            sqlite_db: repo.sqlite_db(),
        };
        let identity = ToolCallIdentity {
            card_id: planner_card.id.as_str().to_string(),
            role: CardRole::Planner,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: "non-root-session".to_string(),
            track_id: Some(track.id.as_str().to_string()),
            area_id: area.id.as_str().to_string(),
            thread_id: "non-root-thread".to_string(),
        };
        let next = TrackReportPayload::new("non-root summary", "# Goal\n\nnon-root body\n");
        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_registry().with(RecorderAdmissionWarnLayer {
            hits: Arc::clone(&warnings),
        });
        let _guard = tracing::subscriber::set_default(subscriber);
        let before_events = repo
            .events_since(0, i64::MAX)
            .await
            .expect("events before commit")
            .len();
        let before_report = repo
            .card_get(report_card.id.as_str())
            .await
            .expect("report before")
            .expect("report row");

        let err = sink
            .commit_report_op(
                &identity,
                ReportDocOp::WriteMarkdown {
                    summary: Some(next.summary),
                    body: next.body,
                    if_doc_rev: 0,
                },
                Some("non-root edit".into()),
            )
            .await
            .expect_err("non-root report write must be forbidden");

        assert!(
            matches!(
                err,
                CalmError::Forbidden(ref message) if message.contains("has no session row")
            ),
            "expected the recorder gate's session-resolution denial, got {err:?}"
        );
        assert_eq!(warnings.load(Ordering::Relaxed), 1);

        let after_report = repo
            .card_get(before_report.id.as_str())
            .await
            .expect("report after")
            .expect("report row");
        assert_eq!(after_report.payload, before_report.payload);
        let events = repo.events_since(0, i64::MAX).await.expect("events");
        assert_eq!(events.len(), before_events);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_report_write_succeeds_under_recorder_enforce() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "recorder-root".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .expect("create area");
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "root track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create track");
        let planner_card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"planner_provider":"codex"}),
            })
            .await
            .expect("create planner card");
        let report_card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial())
                    .expect("initial report payload"),
            })
            .await
            .expect("create report card");

        let root_session_id = WorkerSessionId::from("root-session");
        seed_track_root_session(repo.as_ref(), &track.id, &planner_card.id, &root_session_id).await;
        // The recorder gate reads `cards.role` in-tx; `card_create` persists `worker` regardless of kind.
        sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
            .bind(planner_card.id.as_str())
            .execute(repo.pool())
            .await
            .expect("persist planner card role");

        let card_role_cache = CardRoleCache::new();
        card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
        card_role_cache.insert(
            report_card.id.clone(),
            CardRole::ReportCard,
            track.id.clone(),
        );
        let track_area_cache = TrackAreaCache::new();
        repo.seed_track_area_cache(&track_area_cache)
            .await
            .expect("seed track area cache");
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let sink = CardDecisionSink {
            repo: route_repo,
            events: EventBus::new(),
            write: WriteContext::new(card_role_cache, track_area_cache),
            sqlite_db: repo.sqlite_db(),
        };
        let identity = ToolCallIdentity {
            card_id: planner_card.id.as_str().to_string(),
            role: CardRole::Planner,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: root_session_id.as_str().to_string(),
            track_id: Some(track.id.as_str().to_string()),
            area_id: area.id.as_str().to_string(),
            thread_id: "root-thread".to_string(),
        };
        let next = TrackReportPayload::new("root summary", "# Goal\n\nroot body\n");

        let updated = sink
            .commit_report_op(
                &identity,
                ReportDocOp::WriteMarkdown {
                    summary: Some(next.summary),
                    body: next.body,
                    if_doc_rev: 0,
                },
                Some("root edit".into()),
            )
            .await
            .expect("root report write succeeds")
            .card;

        let payload: TrackReportPayload =
            serde_json::from_value(updated.payload).expect("updated report payload");
        assert_eq!(payload.summary, "root summary");
        let events = repo.events_since(0, i64::MAX).await.expect("events");
        assert!(
            events
                .iter()
                .any(|(_, _, _, event)| matches!(event, Event::TrackReportEdited { .. }))
        );
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
