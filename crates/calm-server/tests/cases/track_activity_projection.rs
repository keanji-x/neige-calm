//! #1722 §6 — the `kernel/track/activity` projector, driven through the
//! production writers of every row it reads (design §4.2/§4.3) and asserted
//! on the payload it writes.
//!
//! Fixture routes (which production writer each row comes from):
//! * areas / tracks / cards: `area_create` / `track_create` /
//!   `card_create_with_id_tx` (the card role is explicit);
//! * sessions: `session_start_runtime_tx` — the mint route that links
//!   `cards.session_id` (F2.6/F2.40); exits: `session_commit_exit` (the reaper
//!   / PTY-exit writer, F2.31); a SILENT `UPDATE worker_sessions SET state`
//!   only where the design row says so (`awaiting_input_overlay_with_
//!   exited_session_is_quiet`, `reconcile_clears_stale_working_after_
//!   session_exit`);
//! * `last_thread_status` / `last_turn_completed_ms`: the feeder's own
//!   writer `session_record_activity_by_thread` (F2.25) — for
//!   `waitingOnApproval` there is no production SOURCE (F2.37), but the
//!   writer is the feeder's;
//! * tasks: a report with task fences + `tasks_rebuild_tx` (the projection
//!   route, `pending`), then `task_claim_pending_tx` (`dispatched`),
//!   `task_mark_running_tx` (`running` + `worker_card_id`),
//!   `task_complete_from_worker_tx` / `task_fail_from_worker_tx`,
//!   `task_mark_sub_track_running_tx`, the scheduler's
//!   `reconcile_child_track_task`, and `task_recovery_allocate_tx` +
//!   `tasks_rebuild_tx` for a new attempt; `tasks.child_track_id` is set by
//!   an UPDATE like `tests/scheduler.rs` does (the bootstrap operation is the
//!   only production writer);
//! * `kernel/card/status`: the real `card_fsm` task fed a `claude.hook`
//!   through `log_pure_event` (the ingest route's emit);
//! * `operations` (isolated marker): one hand-inserted row, as
//!   `task_recovery_preparation.rs` does — the driver is the only writer.

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, card_create_with_id_tx, overlay_upsert_tx,
    session_start_runtime_tx, task_claim_pending_tx, task_complete_from_worker_tx,
    task_fail_from_worker_tx, task_mark_running_tx, task_mark_sub_track_running_tx,
    task_recovery_allocate_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessRegistry, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{
    CardRole, NewArea, NewCard, NewOverlay, NewTrack, RequestTheme, TrackLifecycle, TrackPatch,
    now_ms,
};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, SpawnCtx, SqlxOperationRepo,
};
use calm_server::scheduler::Scheduler;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{DaemonClient, WriteContext};
use calm_server::task_context::TaskContextMonitor;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_activity::sql::SessionRow;
use calm_server::track_activity::{
    ActivityPayload, Attention, CardActivity, CardState, ItemKind, ItemSource, Recompute,
    TrackActivityProjector, fold,
};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_lifecycle::{
    apply_requested_transition_in_tx, auto_transition_if_current_in_tx,
};
use calm_server::track_report::{persist_report, resolve_report_for_track, tasks_rebuild_tx};
use calm_truth::validation::OVERLAY_KIND_REGISTRY;
use calm_types::report_blocks::render_fence;
use calm_types::task_recovery::{
    TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE, TaskRecoveryConstraint, TaskRecoveryRequest,
};
use calm_types::track_report::TrackReportPayload;
use calm_types::worker::WorkerSessionId;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fx {
    repo: Arc<SqlxRepo>,
    repo_dyn: Arc<dyn Repo>,
    pool: sqlx::SqlitePool,
    events: EventBus,
    write: WriteContext,
    role_cache: CardRoleCache,
    area_cache: TrackAreaCache,
    harness: HarnessRegistry,
    projector: TrackActivityProjector,
    area_id: String,
}

async fn fx() -> Fx {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let area = repo_dyn
        .area_create(NewArea {
            name: "activity".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let role_cache = CardRoleCache::new();
    let area_cache = TrackAreaCache::new();
    repo_dyn.seed_card_role_cache(&role_cache).await.unwrap();
    repo_dyn.seed_track_area_cache(&area_cache).await.unwrap();
    let write = WriteContext::new(role_cache.clone(), area_cache.clone());
    let harness = HarnessRegistry::new();
    let projector = TrackActivityProjector::new(
        repo_dyn.clone(),
        events.clone(),
        write.clone(),
        harness.clone(),
    )
    .expect("sqlite-backed repo");
    Fx {
        pool: repo.pool().clone(),
        repo,
        repo_dyn,
        events,
        write,
        role_cache,
        area_cache,
        harness,
        projector,
        area_id: area.id.as_str().to_string(),
    }
}

impl Fx {
    async fn track(&self, title: &str) -> String {
        let track = self
            .repo_dyn
            .track_create(NewTrack {
                template_input: None,
                area_id: self.area_id.clone().into(),
                title: title.into(),
                sort: None,
                cwd: "/neige-fixture-workspace".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        // The fixtures plan ungated codex tasks; rule 6 would refuse them.
        self.repo_dyn
            .track_update(
                track.id.as_str(),
                TrackPatch {
                    require_task_gates: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        self.area_cache
            .insert(track.id.clone(), track.area_id.clone());
        track.id.as_str().to_string()
    }

    async fn set_lifecycle(&self, track_id: &str, lifecycle: TrackLifecycle) {
        self.repo_dyn
            .track_update(
                track_id,
                TrackPatch {
                    lifecycle: Some(lifecycle),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    async fn card(&self, track_id: &str, card_id: &str, kind: &str, role: CardRole) -> String {
        let mut tx = self.pool.begin().await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            card_id.to_string(),
            NewCard {
                track_id: TrackId::from(track_id.to_string()),
                title: None,
                kind: kind.into(),
                sort: None,
                payload: json!({}),
            },
            role,
            true,
            self.repo.card_role_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        self.role_cache
            .insert(card.id.clone(), role, TrackId::from(track_id.to_string()));
        card.id.as_str().to_string()
    }

    /// Mint a session through the production route (`cards.session_id`
    /// follows). `handle_state_json = Some(harness snapshot)` makes it a
    /// harness row (backend (i)).
    #[allow(clippy::too_many_arguments)]
    async fn session(
        &self,
        card_id: &str,
        session_id: &str,
        kind: WorkerSessionKind,
        status: WorkerSessionState,
        thread_id: Option<&str>,
        handle_state_json: Option<Value>,
        created_at_ms: i64,
    ) -> String {
        let provider = match kind {
            WorkerSessionKind::ClaudeCard => Some(AgentProvider::Claude),
            WorkerSessionKind::CodexCard | WorkerSessionKind::SharedPlanner => {
                Some(AgentProvider::Codex)
            }
            WorkerSessionKind::Terminal => None,
        };
        let mut tx = self.pool.begin().await.unwrap();
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: session_id.to_string(),
                card_id: card_id.to_string(),
                kind,
                agent_provider: provider,
                status,
                terminal_run_id: None,
                thread_id: thread_id.map(str::to_string),
                session_id: Some(format!("native-{session_id}")),
                active_turn_id: None,
                handle_state_json,
                spawn_op_id: None,
                now_ms: created_at_ms,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        runtime.id
    }

    fn harness_snapshot() -> Value {
        serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap()
    }

    /// The exit writer (reaper / PTY exit / restart compensation, F2.31):
    /// `state`, `liveness='exited'`, `completed_at_ms = updated_at_ms = at`.
    async fn exit_session(&self, session_id: &str, to: WorkerSessionState, at_ms: i64) {
        self.repo_dyn
            .session_commit_exit(
                &WorkerSessionId::from(session_id),
                to,
                at_ms,
                None,
                "fixture-exit",
            )
            .await
            .unwrap();
    }

    /// The feeder's writer: `last_activity_ms` + `last_thread_status`, and
    /// `last_turn_completed_ms` when `turn_completed_ms` is given.
    async fn stamp(&self, thread_id: &str, at_ms: i64, status: &str, done: Option<i64>) {
        self.repo_dyn
            .session_record_activity_by_thread(thread_id, at_ms, status, done)
            .await
            .unwrap();
    }

    /// Plan tasks the way the planner does: the track's report card is
    /// minted with the canonical initial payload, then `persist_report` (the
    /// report edit boundary, author `user` — E7 ignores it) writes one task
    /// fence per declaration and projects them as `pending`. `decls` are
    /// `(key, kind, spawn, gate_json)`.
    async fn plan_tasks(&self, track_id: &str, decls: &[(&str, &str, &str, Option<Value>)]) {
        let mut body = String::new();
        for (key, kind, spawn, gate) in decls {
            let mut declaration = json!({
                "key": key,
                "kind": kind,
                "goal": format!("do {key}"),
                "context": {},
                "depends_on": [],
                "priority": 0,
                "declared_by": "user",
                "spawn": spawn,
                "ready": true,
            });
            match gate {
                Some(gate) => declaration["gate"] = gate.clone(),
                None if *kind != "terminal" => {
                    declaration["no_gate_reason"] = json!("projection fixture");
                }
                None => {}
            }
            body.push_str(&render_fence("task", &declaration));
            body.push_str("\n\n");
        }
        self.repo_dyn
            .card_create(NewCard {
                track_id: TrackId::from(track_id.to_string()),
                title: None,
                kind: "track-report".into(),
                sort: None,
                payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
            })
            .await
            .unwrap();
        let (track, report_card, previous) = resolve_report_for_track(self.repo.as_ref(), track_id)
            .await
            .unwrap();
        let revision = previous.doc_rev;
        persist_report(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            ActorId::User,
            EditAuthor::User,
            track,
            report_card,
            previous,
            TrackReportPayload::new("", body),
            revision,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    }

    fn task_id(track_id: &str, key: &str) -> String {
        format!("{track_id}:{key}")
    }

    /// The frozen report-block closure of a task (the scheduler resolves it
    /// at claim; recovery re-freezes the same refs).
    async fn closure(&self, track_id: &str, key: &str) -> Vec<calm_types::event::TaskContextRef> {
        TaskContextMonitor::new(
            self.repo_dyn.clone(),
            self.events.clone(),
            self.write.clone(),
        )
        .resolve_task_closure(track_id, key)
        .await
        .unwrap()
        .refs
    }

    /// `pending → dispatched` (the scheduler's claim; `worker_card_id` stays
    /// NULL, F2.22).
    async fn claim(&self, track_id: &str, key: &str, at_ms: i64) {
        self.claim_attempt(track_id, key, &Self::task_id(track_id, key), at_ms)
            .await;
    }

    async fn claim_attempt(&self, track_id: &str, key: &str, attempt_id: &str, at_ms: i64) {
        let refs = self.closure(track_id, key).await;
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let n = task_claim_pending_tx(&mut tx, attempt_id, at_ms, &refs, false)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 1, "claim {key}");
    }

    /// `dispatched → running` + `worker_card_id` (the post-spawn stamp).
    async fn mark_running(&self, track_id: &str, key: &str, worker_card_id: &str, at_ms: i64) {
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let n = task_mark_running_tx(
            &mut tx,
            &Self::task_id(track_id, key),
            Some(worker_card_id),
            at_ms,
            i64::MAX,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 1, "mark_running {key}");
    }

    /// The worker's `calm.task.complete` flip (`done` + `finished_at_ms`).
    async fn complete(&self, track_id: &str, key: &str, worker_card_id: &str, at_ms: i64) {
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let n = task_complete_from_worker_tx(
            &mut tx,
            &Self::task_id(track_id, key),
            track_id,
            calm_server::db::sqlite::TaskReporter::Card {
                card_id: worker_card_id,
                owns_key: true,
            },
            at_ms,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 1, "complete {key}");
    }

    /// The worker's `calm.task.fail` flip (`failed` + `finished_at_ms`).
    async fn fail(&self, track_id: &str, key: &str, worker_card_id: &str, at_ms: i64) {
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let n = task_fail_from_worker_tx(
            &mut tx,
            &Self::task_id(track_id, key),
            track_id,
            calm_server::db::sqlite::TaskReporter::Card {
                card_id: worker_card_id,
                owns_key: true,
            },
            "worker-failed: fixture",
            at_ms,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 1, "fail {key}");
    }

    /// A new attempt for a failed key: the recovery allocation (the creation
    /// route `recover_failed_task` calls after admission) + the projection
    /// rebuild that inserts the new `pending` row. Returns the new attempt id.
    async fn recover(&self, track_id: &str, key: &str, previous_attempt_id: &str) -> String {
        let request = TaskRecoveryRequest {
            expected_attempt_id: previous_attempt_id.to_string(),
            idempotency_key: format!("recover-{key}"),
            reason: "fixture recovery".into(),
        };
        let constraint = TaskRecoveryConstraint::V1 {
            refs: self.closure(track_id, key).await,
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        };
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let receipt = task_recovery_allocate_tx(
            &mut tx,
            track_id,
            key,
            &request,
            "fixture-fingerprint",
            &constraint,
            &ActorId::User,
        )
        .await
        .unwrap();
        tasks_rebuild_tx(&mut tx, track_id).await.unwrap();
        tx.commit().await.unwrap();
        receipt.attempt_id
    }

    async fn task_status(&self, attempt_id: &str) -> (String, Option<String>, Option<i64>) {
        sqlx::query_as("SELECT status, worker_card_id, finished_at_ms FROM tasks WHERE id = ?1")
            .bind(attempt_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Emit a hook exactly as `/internal/claude/hook` does: `log_pure_event`
    /// with the card-level actor and card scope. The real `card_fsm` task
    /// (spawned by [`Fx::spawn_fsm`]) projects it onto `kernel/card/status`.
    async fn claude_hook(
        &self,
        track_id: &str,
        card_id: &str,
        (event_name, snake): (&str, &str),
        payload: Value,
    ) {
        let kind = format!("hook.claude.{snake}");
        let mut payload = payload;
        payload["hook_event_name"] = json!(event_name);
        self.repo_dyn
            .log_pure_event(
                ActorId::AiClaude(CardId::from(card_id.to_string())),
                EventScope::Card {
                    card: CardId::from(card_id.to_string()),
                    track: TrackId::from(track_id.to_string()),
                    area: self.area_id.clone().into(),
                },
                None,
                &self.events,
                &self.role_cache,
                &self.area_cache,
                Event::ClaudeHook {
                    card_id: CardId::from(card_id.to_string()),
                    kind,
                    hook_idempotency_key: format!("{card_id}-{event_name}-{}", now_ms()),
                    payload,
                },
            )
            .await
            .unwrap();
    }

    fn spawn_fsm(&self) {
        calm_server::card_fsm::spawn(
            self.repo_dyn.clone(),
            self.events.clone(),
            self.write.clone(),
        );
    }

    async fn await_card_status(&self, card_id: &str, expected: &str) -> i64 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let overlays = self.repo_dyn.overlays_for("card", card_id).await.unwrap();
            if let Some(o) = overlays.iter().find(|o| o.kind == "status")
                && o.payload.get("state").and_then(Value::as_str) == Some(expected)
            {
                return o.updated_at;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for kernel/card/status = {expected} on {card_id}: {overlays:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Install a LIVE (unstarted) harness handle for `session_id` in the
    /// registry — backend (i)'s in-process witness.
    async fn install_live_harness(&self, track_id: &str, card_id: &str, session_id: &str) {
        let daemon = SharedCodexAppServer::new_stub(self.repo_dyn.clone());
        let (handle, _obs) = PlannerHarness::run_unstarted_for_test(
            PlannerHarnessParams {
                worker_session_id: session_id.to_string(),
                track_id: TrackId::from(track_id.to_string()),
                card_id: CardId::from(card_id.to_string()),
                thread_id: None,
                repo: self.repo_dyn.clone(),
                events: self.events.clone(),
                card_role_cache: self.role_cache.clone(),
                track_area_cache: self.area_cache.clone(),
                daemon,
                config: HarnessConfig::default(),
                snapshot: HarnessSnapshot::initial(0, vec![]),
            },
            8,
        );
        self.harness.insert(session_id.to_string(), handle);
    }

    /// A scheduler over this repo, for `reconcile_child_track_task`.
    fn scheduler(&self) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
        let operation_repo = Arc::new(SqlxOperationRepo::new(self.pool.clone()));
        let route_repo: Arc<dyn calm_server::db::RouteRepo> = self.repo.clone();
        let completion = OperationCompletionBus::new();
        let spawn_ctx = SpawnCtx::new(
            route_repo,
            operation_repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new(),
            self.events.clone(),
            completion.clone(),
        );
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo,
            vec![],
            self.events.clone(),
            completion,
            spawn_ctx,
        ));
        let scheduler = Scheduler::new(
            self.repo_dyn.clone(),
            self.events.clone(),
            self.write.clone(),
            Arc::downgrade(&runtime),
            Arc::new(tokio::sync::Semaphore::new(4)),
        );
        (runtime, scheduler)
    }

    /// Seed a `kernel/track/activity` row directly (the projector's own
    /// writer shape) — for the monotone high-water-mark test.
    async fn seed_activity_overlay(&self, track_id: &str, payload: Value) {
        let overlay = NewOverlay {
            plugin_id: "kernel".into(),
            entity_kind: "track".into(),
            entity_id: track_id.to_string(),
            kind: "activity".into(),
            payload,
        };
        write_with_event_typed(
            self.repo_dyn.as_ref(),
            ActorId::Kernel,
            EventScope::Track {
                track: TrackId::from(track_id.to_string()),
                area: self.area_id.clone().into(),
            },
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let o = overlay_upsert_tx(tx, overlay).await?;
                    Ok(((), Event::OverlaySet(o)))
                })
            },
        )
        .await
        .unwrap();
    }

    async fn recompute(&self, track_id: &str) -> ActivityPayload {
        match self.projector.recompute_track(track_id).await.unwrap() {
            Recompute::NoTrack => panic!("track {track_id} vanished"),
            Recompute::Unchanged(p) | Recompute::Written(p) => p,
        }
    }

    async fn stored(&self, track_id: &str) -> Option<ActivityPayload> {
        self.repo_dyn
            .overlays_for("track", track_id)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "activity" && o.plugin_id == "kernel")
            .map(|o| serde_json::from_value(o.payload).unwrap())
    }
}

fn card_state(p: &ActivityPayload, card_id: &str) -> Option<CardState> {
    p.cards
        .iter()
        .find(|c| c.card_id == card_id)
        .map(|c| c.state)
}

fn quiet(p: &ActivityPayload) -> bool {
    !p.working && p.attention == Attention::None && p.items.is_empty() && p.cards.is_empty()
}

// ---------------------------------------------------------------------------
// W — the task clause (§4.2 W)
// ---------------------------------------------------------------------------

/// `dispatched` (worker card NULL, session `starting`, no thread status)
/// → track `working`, no `cards[]` entry; the post-spawn `running` stamp
/// → `cards[worker] = working` (§7 C3 / C3′).
#[tokio::test]
async fn dispatched_task_is_working_without_session_signal() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Starting,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    let before = f.recompute(&t).await;
    assert!(quiet(&before), "a pending task is not working: {before:?}");

    f.claim(&t, "build", 2_000).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "dispatched ⇒ working: {p:?}");
    assert_eq!(p.attention, Attention::None);
    assert!(
        p.cards.is_empty(),
        "no worker card is stamped while dispatched (F2.22): {p:?}"
    );

    f.mark_running(&t, "build", &worker, 3_000).await;
    let p = f.recompute(&t).await;
    assert!(p.working);
    assert_eq!(
        p.cards,
        vec![CardActivity {
            card_id: worker.clone(),
            state: CardState::Working
        }]
    );
    assert_eq!(p.activity_at_ms, None, "nothing has completed yet");
}

/// F2.29: a shared-daemon worker rests at `running` + `active` after its
/// task is `done` — the task row decides, the session does not.
#[tokio::test]
async fn completed_worker_with_stale_active_status_is_not_working() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.stamp("th-w", 3_500, "active", None).await;
    assert!(f.recompute(&t).await.working);

    f.complete(&t, "build", &worker, 4_000).await;
    // The thread keeps reporting `active` after the report (F2.29/F2.36).
    f.stamp("th-w", 4_050, "active", None).await;
    let p = f.recompute(&t).await;
    assert!(
        !p.working,
        "done task ⇒ not working despite `active`: {p:?}"
    );
    assert_eq!(p.attention, Attention::None);
    assert!(p.cards.is_empty());
    assert_eq!(p.activity_at_ms, Some(4_000), "E3 = finished_at_ms");
}

/// `child_track_id` rows in `running` are the CHILD's work (G20): the
/// parent shows nothing while the child's planner idles.
#[tokio::test]
async fn sub_track_parent_running_with_idle_child_is_not_working() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    f.set_lifecycle(&child, TrackLifecycle::Planning).await;
    let planner = f
        .card(&child, "card-child-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "ws-child-planner",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("th-child"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    f.install_live_harness(&child, &planner, &ws).await;
    f.plan_tasks(&parent, &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, None)])
        .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    let n = task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(n, 1);
    let (status, worker, _) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("running", None));

    let p = f.recompute(&parent).await;
    assert!(
        !p.working,
        "a running sub-track row is not the parent's work: {p:?}"
    );
    assert!(p.cards.is_empty());
    assert_eq!(p.attention, Attention::None);
    // The child itself: idle planner ⇒ not working either (§7 A0).
    assert!(!f.recompute(&child).await.working);
}

/// Twin: the child fails ⇒ `reconcile_child_track_task` fails the parent
/// row ⇒ parent `attention = failed`, one `task` item with `card_id: null`.
#[tokio::test]
async fn sub_track_child_failed_marks_parent_failed() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    f.plan_tasks(&parent, &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, None)])
        .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.set_lifecycle(&child, TrackLifecycle::Failed).await;
    let (_runtime, scheduler) = f.scheduler();
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let (status, worker, finished) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("failed", None));

    let p = f.recompute(&parent).await;
    assert!(!p.working);
    assert_eq!(p.attention, Attention::Failed);
    assert_eq!(p.items.len(), 1, "{p:?}");
    let item = &p.items[0];
    assert_eq!(item.kind, ItemKind::Failed);
    assert_eq!(item.source, ItemSource::Task);
    assert_eq!(item.id, "sub");
    assert_eq!(item.card_id, None);
    assert_eq!(Some(item.at_ms), finished);
    assert!(p.cards.is_empty(), "no worker card ⇒ no cards[] entry");
    assert_eq!(p.activity_at_ms, finished, "E3 counts the failure");
}

/// F2.39: the child is `done` and quiescent, the parent row carries a
/// gate ⇒ `verifying` (still `child_track_id`, `worker_card_id` NULL) — the
/// PARENT's own gate is running ⇒ `working`, no `cards[]` entry.
#[tokio::test]
async fn parent_gate_verifying_is_working() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    let gate = json!({ "cwd": "/tmp", "steps": [{ "name": "ok", "cmd": "true" }] });
    f.plan_tasks(
        &parent,
        &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, Some(gate))],
    )
    .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.set_lifecycle(&child, TrackLifecycle::Done).await;
    let (_runtime, scheduler) = f.scheduler();
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let (status, worker, _) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("verifying", None));
    let child_link: Option<String> =
        sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id = ?1")
            .bind(Fx::task_id(&parent, "sub"))
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        child_link.as_deref(),
        Some(child.as_str()),
        "the flip keeps child_track_id"
    );

    let p = f.recompute(&parent).await;
    assert!(p.working, "verifying is the parent's gate: {p:?}");
    assert!(p.cards.is_empty());
    assert_eq!(p.attention, Attention::None);
}

// ---------------------------------------------------------------------------
// (ii) shared-daemon interactive cards
// ---------------------------------------------------------------------------

/// A never-task-bound `codex-create` card: `running` + `active` ⇒ working.
#[tokio::test]
async fn interactive_codex_card_active_is_working() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    f.session(
        &card,
        "ws-i",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-i"),
        None,
        1_000,
    )
    .await;
    f.stamp("th-i", 2_000, "active", None).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(card_state(&p, &card), Some(CardState::Working));
    assert_eq!(p.attention, Attention::None);
    assert_eq!(p.activity_at_ms, None);
}

/// `last_thread_status IS NULL` (the mint value) is not a signal.
#[tokio::test]
async fn null_thread_status_is_quiet() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    f.session(
        &card,
        "ws-i",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-i"),
        None,
        1_000,
    )
    .await;
    let p = f.recompute(&t).await;
    assert!(quiet(&p), "{p:?}");
}

/// The feeder's fail-closed `"unknown"` (§4.2.1) is outside every predicate.
#[tokio::test]
async fn unknown_thread_status_is_quiet() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    f.session(
        &card,
        "ws-i",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-i"),
        None,
        1_000,
    )
    .await;
    f.stamp("th-i", 2_000, "unknown", None).await;
    let p = f.recompute(&t).await;
    assert!(quiet(&p), "{p:?}");
    // …and `idle` after a completed turn is quiet too (the §7 B′ resting
    // value), with E6 lighting the completion.
    f.stamp("th-i", 3_000, "idle", Some(3_000)).await;
    let p = f.recompute(&t).await;
    assert!(!p.working);
    assert_eq!(p.attention, Attention::None);
    assert!(p.cards.is_empty());
    assert_eq!(p.activity_at_ms, Some(3_000), "E6");
}

/// §7 B′ (fixture-only source, F2.37): a running worker whose thread waits
/// on approval ⇒ `attention = input`, `cards[card] = input` (folded over
/// W's `working`), one `session` item with `card_id`; back to `active` ⇒
/// working; `task.completed` ⇒ unread (E3).
#[tokio::test]
async fn waiting_on_approval_is_input() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-w",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-w"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.stamp("th-w", 3_500, "waitingOnApproval", None).await;

    let p = f.recompute(&t).await;
    assert!(p.working, "W still holds: {p:?}");
    assert_eq!(p.attention, Attention::Input);
    assert_eq!(card_state(&p, &worker), Some(CardState::Input));
    assert_eq!(p.items.len(), 1);
    let item = &p.items[0];
    assert_eq!(item.kind, ItemKind::Input);
    assert_eq!(item.source, ItemSource::Session);
    assert_eq!(item.id, ws);
    assert_eq!(item.card_id.as_deref(), Some(worker.as_str()));
    assert_eq!(
        item.at_ms, 3_500,
        "a status item carries the feeder stamp time"
    );

    f.stamp("th-w", 4_000, "active", None).await;
    let p = f.recompute(&t).await;
    assert!(p.working);
    assert_eq!(p.attention, Attention::None);
    assert_eq!(card_state(&p, &worker), Some(CardState::Working));

    f.complete(&t, "build", &worker, 5_000).await;
    let p = f.recompute(&t).await;
    assert!(!p.working);
    assert_eq!(p.attention, Attention::None);
    assert_eq!(p.activity_at_ms, Some(5_000));
}

/// `waitingOnUserInput` on an INTERACTIVE card is input; `systemError` on a
/// live session is failed (backend (ii) attention / failed columns).
#[tokio::test]
async fn interactive_codex_thread_status_input_and_system_error() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    f.session(
        &card,
        "ws-i",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-i"),
        None,
        1_000,
    )
    .await;
    f.stamp("th-i", 2_000, "waitingOnUserInput", None).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Input);
    assert_eq!(card_state(&p, &card), Some(CardState::Input));
    assert!(!p.working);

    f.stamp("th-i", 3_000, "systemError", None).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed);
    assert_eq!(card_state(&p, &card), Some(CardState::Failed));
    assert_eq!(p.items[0].at_ms, 3_000);
}

/// An interactive card whose session died `failed` (signal-killed / spawn
/// compensation) is red until the card is restarted or deleted.
#[tokio::test]
async fn interactive_card_failed_session_is_failed() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &card,
            "ws-i",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-i"),
            None,
            1_000,
        )
        .await;
    f.exit_session(&ws, WorkerSessionState::Failed, 5_000).await;
    let p = f.recompute(&t).await;
    assert!(!p.working);
    assert_eq!(p.attention, Attention::Failed);
    assert_eq!(card_state(&p, &card), Some(CardState::Failed));
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Session);
    assert_eq!(p.items[0].id, ws);
    assert_eq!(p.items[0].card_id.as_deref(), Some(card.as_str()));
    assert_eq!(
        p.items[0].at_ms, 5_000,
        "a failed-state item carries the exit time"
    );
}

/// (iii) the isolated marker is on the CARD (`operations.target_id`), not on
/// `ws.spawn_op_id`: a re-minted isolated session (`spawn_op_id NULL`) with
/// `waitingOnApproval` has no attention rule — judged by `spawn_op_id` it
/// would fall into (ii) and light `input`.
#[tokio::test]
async fn reminted_isolated_session_is_not_shared_daemon() {
    let f = fx().await;
    let t = f.track("iso").await;
    let card = f.card(&t, "card-iso", "codex", CardRole::Worker).await;
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,\
         target_id,target_json,payload_json,phase,created_at_ms,updated_at_ms) \
         VALUES('op-iso','op-iso',?1,'iso-task','h','card',?2,'{}','{}','succeeded',1,1)",
    )
    .bind(calm_server::isolated_codex::OPERATION_KIND)
    .bind(&card)
    .execute(&f.pool)
    .await
    .unwrap();
    f.session(
        &card,
        "ws-iso",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-iso"),
        None,
        1_000,
    )
    .await;
    f.stamp("th-iso", 2_000, "waitingOnApproval", None).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "{p:?}");
    assert!(p.items.is_empty());
    // …and `active` is not `working` for it either (W only, F2.33).
    f.stamp("th-iso", 3_000, "active", None).await;
    assert!(!f.recompute(&t).await.working);
}

// ---------------------------------------------------------------------------
// (i) harness rows — registry-gated `working`
// ---------------------------------------------------------------------------

async fn harness_track(f: &Fx, state: WorkerSessionState) -> (String, String, String) {
    let t = f.track("plan").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let planner = f
        .card(&t, "card-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "ws-planner",
            WorkerSessionKind::SharedPlanner,
            state,
            Some("th-planner"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    (t, planner, ws)
}

/// A `turn_pending` row with NO live handle (boot sweep before
/// `boot_harnesses`, or a crashed loop) is not working.
#[tokio::test]
async fn turn_pending_row_without_live_harness_is_not_working() {
    let f = fx().await;
    let (t, _planner, _ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    let p = f.recompute(&t).await;
    assert!(!p.working, "{p:?}");
    assert!(p.cards.is_empty());
}

/// Twin: the same row with its handle installed IS working (§7 A1).
#[tokio::test]
async fn turn_pending_row_with_live_harness_is_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(card_state(&p, &planner), Some(CardState::Working));
}

/// §1's symptom: `planning` with an `idle` planner is NOT working, however
/// live the run loop is.
#[tokio::test]
async fn planning_track_with_idle_planner_is_not_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(quiet(&p), "{p:?}");
}

/// Q4: `starting` is not working even with a live handle.
#[tokio::test]
async fn starting_harness_is_not_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Starting).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(!p.working, "{p:?}");
    assert!(p.cards.is_empty());
}

/// A wedged harness (`state='failed'`, F2.3) is a `session` failed item.
#[tokio::test]
async fn wedged_harness_is_failed() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 9_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed);
    assert_eq!(card_state(&p, &planner), Some(CardState::Failed));
    assert_eq!(p.items[0].source, ItemSource::Session);
    assert_eq!(p.items[0].id, ws);
}

// ---------------------------------------------------------------------------
// (iv) claude PTY — FSM rows behind the live-session gate
// ---------------------------------------------------------------------------

async fn claude_interactive(f: &Fx) -> (String, String, String) {
    let t = f.track("claude").await;
    let card = f.card(&t, "card-c", "claude", CardRole::Worker).await;
    let ws = f
        .session(
            &card,
            "ws-c",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Running,
            None,
            None,
            1_000,
        )
        .await;
    (t, card, ws)
}

#[tokio::test]
async fn awaiting_input_overlay_with_running_session_is_input() {
    let f = fx().await;
    f.spawn_fsm();
    let (t, card, _ws) = claude_interactive(&f).await;
    f.claude_hook(
        &t,
        &card,
        ("PermissionRequest", "permission_request"),
        json!({}),
    )
    .await;
    let at = f.await_card_status(&card, "AwaitingInput").await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Input, "{p:?}");
    assert_eq!(card_state(&p, &card), Some(CardState::Input));
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Card);
    assert_eq!(p.items[0].id, card);
    assert_eq!(p.items[0].card_id.as_deref(), Some(card.as_str()));
    assert_eq!(
        p.items[0].at_ms, at,
        "a card item carries overlays.updated_at"
    );
    assert!(!p.working);
}

/// The live-session gate: the FSM row is untouched, the session leaves
/// silently (no event, F2.31) ⇒ the next reconcile ignores the row.
#[tokio::test]
async fn awaiting_input_overlay_with_exited_session_is_quiet() {
    let f = fx().await;
    f.spawn_fsm();
    let (t, card, ws) = claude_interactive(&f).await;
    f.claude_hook(
        &t,
        &card,
        ("PermissionRequest", "permission_request"),
        json!({}),
    )
    .await;
    f.await_card_status(&card, "AwaitingInput").await;
    assert_eq!(f.recompute(&t).await.attention, Attention::Input);

    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = ?1")
        .bind(&ws)
        .execute(&f.pool)
        .await
        .unwrap();
    f.projector.reconcile_all().await;
    let p = f.stored(&t).await.unwrap();
    assert_eq!(p.attention, Attention::None, "{p:?}");
    assert!(p.cards.is_empty());
    assert!(p.items.is_empty());
    // The FSM row itself was not rewritten (Q2 = (c)).
    assert!(f.await_card_status(&card, "AwaitingInput").await > 0);
}

/// An interactive claude card in FSM `Working` with a live session is
/// working; `Errored` is failed.
#[tokio::test]
async fn interactive_claude_fsm_working_and_errored() {
    let f = fx().await;
    f.spawn_fsm();
    let (t, card, _ws) = claude_interactive(&f).await;
    f.claude_hook(&t, &card, ("PreToolUse", "pre_tool_use"), json!({}))
        .await;
    f.await_card_status(&card, "Working").await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(card_state(&p, &card), Some(CardState::Working));

    f.claude_hook(&t, &card, ("StopFailure", "stop_failure"), json!({}))
        .await;
    f.await_card_status(&card, "Errored").await;
    let p = f.recompute(&t).await;
    assert!(!p.working);
    assert_eq!(p.attention, Attention::Failed);
    assert_eq!(card_state(&p, &card), Some(CardState::Failed));
}

// ---------------------------------------------------------------------------
// The † exception (G19) and the S0 eligibility fence (B-M5)
// ---------------------------------------------------------------------------

/// Task `done@t1`, worker session minted at `t0 < t1`, later signal-killed
/// (`failed`, F2.31) ⇒ quiet: the verdict belongs to finished work.
#[tokio::test]
async fn done_task_worker_signal_killed_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "claude", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-w",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Running,
            None,
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "claude", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 6_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "{p:?}");
    assert!(p.cards.is_empty());
    assert!(p.items.is_empty());
    assert!(!p.working);
    assert_eq!(p.activity_at_ms, Some(4_000));
}

/// Twin: the same done task, session still running, a permission prompt
/// from work the user added in the same PTY ⇒ still `input`.
#[tokio::test]
async fn done_task_worker_permission_prompt_is_input() {
    let f = fx().await;
    f.spawn_fsm();
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "claude", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::ClaudeCard,
        WorkerSessionState::Running,
        None,
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "claude", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    f.claude_hook(
        &t,
        &worker,
        ("PermissionRequest", "permission_request"),
        json!({}),
    )
    .await;
    f.await_card_status(&worker, "AwaitingInput").await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Input, "{p:?}");
    assert_eq!(card_state(&p, &worker), Some(CardState::Input));
    assert!(
        !p.working,
        "the done task does not work; the prompt is input"
    );
}

/// B-MAJOR-1 (v6): the task is `done@t1`; a restart AFTER that mints S2
/// (`created_at_ms = t2 > t1`, `cards.session_id = S2`, F2.40); the
/// replacement spawn fails and compensation writes S2 `failed` ⇒ red. This
/// is new work's failure, not the finished task's exit.
#[tokio::test]
async fn done_task_replacement_session_failure_is_failed() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "claude", CardRole::Worker).await;
    let s1 = f
        .session(
            &worker,
            "ws-s1",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Running,
            None,
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "claude", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    // The restart: the still-active S1 is written `exited`, S2 is minted
    // with the current clock and takes over `cards.session_id`.
    f.exit_session(&s1, WorkerSessionState::Exited, 5_000).await;
    let s2 = f
        .session(
            &worker,
            "ws-s2",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Starting,
            None,
            None,
            6_000,
        )
        .await;
    let current: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(&worker)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(current.as_deref(), Some(s2.as_str()));
    // Compensation for the failed replacement spawn.
    f.exit_session(&s2, WorkerSessionState::Failed, 7_000).await;

    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed, "{p:?}");
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Session);
    assert_eq!(p.items[0].id, s2);
    assert_eq!(p.items[0].at_ms, 7_000);
}

/// (1) attempt A's worker session stays `failed` (the reaper's exit
/// writer), recovery allocates B, B completes ⇒ `none`, `activity_at` = B's
/// `finished_at_ms`. (2) the empty set: the fold applied to A's session
/// with NO current row for A's card must NOT suppress its `failed` — the
/// exception needs at least one `done` row (v7 A-MIN3), so an S0 fence
/// removed together with a vacuous `all()` cannot stay green.
#[tokio::test]
async fn superseded_failed_attempt_session_is_not_actionable() {
    let f = fx().await;
    let t = f.track("w").await;
    let card_a = f.card(&t, "card-a", "codex", CardRole::Worker).await;
    let ws_a = f
        .session(
            &card_a,
            "ws-a",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-a"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &card_a, 3_000).await;
    f.fail(&t, "build", &card_a, 4_000).await;
    f.exit_session(&ws_a, WorkerSessionState::Failed, 4_500)
        .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed, "attempt A failed: {p:?}");
    assert_eq!(p.activity_at_ms, Some(4_000));

    let attempt_b = f.recover(&t, "build", &Fx::task_id(&t, "build")).await;
    let card_b = f.card(&t, "card-b", "codex", CardRole::Worker).await;
    f.session(
        &card_b,
        "ws-b",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-b"),
        None,
        5_000,
    )
    .await;
    f.claim_attempt(&t, "build", &attempt_b, 6_000).await;
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    assert_eq!(
        task_mark_running_tx(&mut tx, &attempt_b, Some(&card_b), 7_000, i64::MAX)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        task_complete_from_worker_tx(
            &mut tx,
            &attempt_b,
            &t,
            calm_server::db::sqlite::TaskReporter::Card {
                card_id: &card_b,
                owns_key: true,
            },
            8_000,
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    // A's row is still `failed` in `tasks`, just not current (F2.22).
    assert_eq!(f.task_status(&Fx::task_id(&t, "build")).await.0, "failed");

    let p = f.recompute(&t).await;
    assert_eq!(
        p.attention,
        Attention::None,
        "(1) A's failed session is fenced: {p:?}"
    );
    assert!(p.items.is_empty());
    assert!(p.cards.is_empty());
    assert!(!p.working);
    assert_eq!(p.activity_at_ms, Some(8_000));

    // (2) the empty set. Read the real rows, then hand the fold A's session
    // as if the S0 fence had admitted it: no current row names card A, so
    // the exception does not hold and A's `failed` is a failed item.
    let rows = f.projector.read_rows(&t).await.unwrap().unwrap();
    assert!(
        rows.sessions.iter().all(|s| s.id != ws_a),
        "S0 fences A's session out: {:?}",
        rows.sessions
    );
    assert!(
        rows.tasks
            .iter()
            .all(|task| task.worker_card_id.as_deref() != Some(card_a.as_str())),
        "no current row names card A"
    );
    let mut admitted = rows.clone();
    admitted.sessions.push(SessionRow {
        id: ws_a.clone(),
        card_id: card_a.clone(),
        provider: "codex".into(),
        state: "failed".into(),
        last_thread_status: None,
        last_activity_ms: None,
        updated_at_ms: 4_500,
        created_at_ms: 1_000,
        mode: None,
        isolated: false,
        task_bound: true,
    });
    let folded = fold(&t, &admitted);
    assert_eq!(
        folded.attention(),
        Attention::Failed,
        "(2) an empty current-row set never suppresses a failed session: {folded:?}"
    );
    assert!(
        folded
            .items
            .iter()
            .any(|i| i.source == ItemSource::Session && i.id == ws_a)
    );
}

// ---------------------------------------------------------------------------
// Completion-class evidence and the monotone high-water mark (§4.3)
// ---------------------------------------------------------------------------

/// E5/E6 carry the "never task-bound" predicate: a worker's later turn end
/// (`last_turn_completed_ms = t2 > t1`) and a `stop` hook on the worker
/// card do NOT relight a result E3 already lit at `t1`.
#[tokio::test]
async fn worker_turn_end_after_task_done_does_not_relight() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    let t1 = now_ms() - 10_000;
    f.complete(&t, "build", &worker, t1).await;
    let t2 = t1 + 5_000;
    f.stamp("th-w", t2, "idle", Some(t2)).await;
    // A stop hook on the worker card, persisted at `now` (> t1).
    f.claude_hook(&t, &worker, ("Stop", "stop"), json!({}))
        .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(t1), "one result, one unread: {p:?}");
}

/// Twin: the same columns on a never-task-bound card DO light: E6 alone
/// gives `t2`; a stop hook on a second interactive card gives the event's
/// own `at`.
#[tokio::test]
async fn interactive_card_turn_end_lights_unread() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    f.session(
        &card,
        "ws-i",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-i"),
        None,
        1_000,
    )
    .await;
    let t2 = now_ms() - 5_000;
    f.stamp("th-i", t2, "idle", Some(t2)).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(t2), "E6: {p:?}");

    let claude = f.card(&t, "card-c", "claude", CardRole::Worker).await;
    f.session(
        &claude,
        "ws-c",
        WorkerSessionKind::ClaudeCard,
        WorkerSessionState::Running,
        None,
        None,
        1_000,
    )
    .await;
    f.claude_hook(&t, &claude, ("Stop", "stop"), json!({}))
        .await;
    let at: i64 = sqlx::query_scalar(
        "SELECT MAX(at) FROM events WHERE kind = 'claude.hook' AND scope_track = ?1",
    )
    .bind(&t)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(at), "E5: {p:?}");
    assert!(at > t2);
}

/// E4's actor filter: the user's own `draft → planning` does not move the
/// high-water mark.
#[tokio::test]
async fn user_lifecycle_edge_does_not_advance_activity() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    assert_eq!(f.recompute(&t).await.activity_at_ms, Some(4_000));

    let track_id = TrackId::from(t.clone());
    let area = f.area_id.clone();
    write_with_event_typed::<(), _>(
        f.repo_dyn.as_ref(),
        ActorId::User,
        EventScope::Track {
            track: track_id.clone(),
            area: area.into(),
        },
        None,
        &f.events,
        &f.write,
        move |tx| {
            Box::pin(async move {
                let events = apply_requested_transition_in_tx(
                    tx,
                    &track_id,
                    TrackLifecycle::Planning,
                    &ActorId::User,
                    "kick off".into(),
                )
                .await?
                .expect("draft → planning by the user");
                Ok(((), events.into_iter().next().unwrap()))
            })
        },
    )
    .await
    .unwrap();
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(4_000), "{p:?}");
    assert!(!p.working, "planning is a phase, not activity");
}

/// Positive twin: `working → reviewing` by `KernelDispatcher` through
/// `auto_transition_if_current_in_tx` (the gate-result adapter's shape)
/// advances it to the event's `at`.
#[tokio::test]
async fn kernel_lifecycle_edge_advances_activity() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    assert_eq!(f.recompute(&t).await.activity_at_ms, Some(4_000));

    let track_id = TrackId::from(t.clone());
    let area = f.area_id.clone();
    write_with_event_typed::<(), _>(
        f.repo_dyn.as_ref(),
        ActorId::KernelDispatcher,
        EventScope::Track {
            track: track_id.clone(),
            area: area.into(),
        },
        None,
        &f.events,
        &f.write,
        move |tx| {
            Box::pin(async move {
                let events = auto_transition_if_current_in_tx(
                    tx,
                    &track_id,
                    TrackLifecycle::Working,
                    TrackLifecycle::Reviewing,
                    &ActorId::KernelDispatcher,
                    None,
                )
                .await?
                .expect("working → reviewing by the kernel");
                Ok(((), events.into_iter().next().unwrap()))
            })
        },
    )
    .await
    .unwrap();
    let at: i64 = sqlx::query_scalar(
        "SELECT MAX(at) FROM events WHERE kind = 'track.lifecycle_changed' AND scope_track = ?1",
    )
    .bind(&t)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(at), "E4: {p:?}");
    assert!(at > 4_000);
    // `reviewing` is also a lifecycle `input` item.
    assert_eq!(p.attention, Attention::Input);
    assert_eq!(p.items[0].source, ItemSource::Lifecycle);
    assert_eq!(p.items[0].id, t);
    assert_eq!(p.items[0].card_id, None);
}

/// The tick reads every unarchived track: a task that started and ended
/// between two ticks on a track with no overlay and no delivered event is
/// still found and lit.
#[tokio::test]
async fn quiet_track_short_task_completed_between_ticks_is_unread() {
    let f = fx().await;
    let t = f.track("quiet").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    assert!(
        f.stored(&t).await.is_none(),
        "no overlay before the first tick"
    );

    f.projector.reconcile_all().await;
    let p = f.stored(&t).await.expect("the tick seeds the row");
    assert_eq!(p.activity_at_ms, Some(4_000));
    assert!(!p.working);

    // An archived track leaves the tick set: a later completion there is
    // not picked up by `reconcile_all`.
    let archived = f.track("archived").await;
    let worker2 = f
        .card(&archived, "card-w2", "codex", CardRole::Worker)
        .await;
    f.plan_tasks(&archived, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&archived, "build", 2_000).await;
    f.mark_running(&archived, "build", &worker2, 3_000).await;
    f.complete(&archived, "build", &worker2, 4_000).await;
    sqlx::query("UPDATE tracks SET archived_at = 5000 WHERE id = ?1")
        .bind(&archived)
        .execute(&f.pool)
        .await
        .unwrap();
    f.projector.reconcile_all().await;
    assert!(
        f.stored(&archived).await.is_none(),
        "archived tracks are not ticked"
    );
}

/// Session exits emit no event (F2.4/F2.31): the tick clears a `working`
/// left by an interactive card whose session left silently.
#[tokio::test]
async fn reconcile_clears_stale_working_after_session_exit() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &card,
            "ws-i",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-i"),
            None,
            1_000,
        )
        .await;
    f.stamp("th-i", 2_000, "active", None).await;
    f.projector.reconcile_all().await;
    assert!(f.stored(&t).await.unwrap().working);

    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = ?1")
        .bind(&ws)
        .execute(&f.pool)
        .await
        .unwrap();
    f.projector.reconcile_all().await;
    let p = f.stored(&t).await.unwrap();
    assert!(!p.working, "{p:?}");
    assert!(p.cards.is_empty());
}

/// M10: a seeded high-water mark survives a recomputation that finds no
/// (or only older) evidence.
#[tokio::test]
async fn activity_at_is_monotone() {
    let f = fx().await;
    let t = f.track("w").await;
    let big = 4_000_000_000_000_i64;
    f.seed_activity_overlay(
        &t,
        json!({
            "schemaVersion": 1, "working": true, "attention": "none",
            "activity_at_ms": big, "items": [], "cards": []
        }),
    )
    .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(big), "{p:?}");
    assert!(
        !p.working,
        "the conclusions are recomputed; only the mark is kept"
    );

    // Older evidence arrives: still `big`.
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(big));
    assert_eq!(f.stored(&t).await.unwrap().activity_at_ms, Some(big));
}

// ---------------------------------------------------------------------------
// Write discipline and the registry
// ---------------------------------------------------------------------------

/// An unchanged recomputation writes nothing and emits nothing (F2.19);
/// a change writes exactly one `overlay.set` with the track scope.
#[tokio::test]
async fn unchanged_recompute_emits_no_event() {
    let f = fx().await;
    let t = f.track("w").await;
    let mut rx = f.events.subscribe();
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::Written(_)
    ));
    let env = rx.recv().await.unwrap();
    match &env.event {
        Event::OverlaySet(o) => {
            assert_eq!(
                (o.kind.as_str(), o.entity_kind.as_str()),
                ("activity", "track")
            );
            assert_eq!(o.entity_id, t);
            assert_eq!(o.plugin_id, "kernel");
        }
        other => panic!("expected overlay.set, got {other:?}"),
    }
    assert_eq!(env.scope.track_id().map(|x| x.as_str()), Some(t.as_str()));
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::Unchanged(_)
    ));
    assert!(
        rx.try_recv().is_err(),
        "an unchanged recompute must not emit"
    );
    // A deleted track: nothing to project.
    assert!(matches!(
        f.projector.recompute_track("no-such-track").await.unwrap(),
        Recompute::NoTrack
    ));
}

/// Every payload the projector writes passes the `activity` entry of the
/// overlay kind registry (§4.5), so the wire shape and the validator cannot
/// drift apart.
#[tokio::test]
async fn activity_payload_passes_the_overlay_registry() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Reviewing).await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-w",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-w"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(
        &t,
        &[
            ("build", "codex", TASK_IN_TRACK_ROUTE, None),
            ("test", "codex", TASK_IN_TRACK_ROUTE, None),
        ],
    )
    .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.fail(&t, "build", &worker, 4_000).await;
    f.claim(&t, "test", 5_000).await;
    f.stamp("th-w", 6_000, "waitingOnApproval", None).await;
    let _ = ws;
    let p = f.recompute(&t).await;
    // Every source is represented: task (failed), session (input),
    // lifecycle (input); a card folded to `failed`; working from `test`.
    assert!(p.working);
    assert_eq!(p.attention, Attention::Failed);
    let sources: Vec<ItemSource> = p.items.iter().map(|i| i.source).collect();
    assert!(sources.contains(&ItemSource::Task));
    assert!(sources.contains(&ItemSource::Session));
    assert!(sources.contains(&ItemSource::Lifecycle));
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
    let stored = f
        .repo_dyn
        .overlays_for("track", &t)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "activity")
        .unwrap();
    OVERLAY_KIND_REGISTRY
        .validate("activity", &stored.payload)
        .expect("the projector's payload is the registry's shape");
    assert_eq!(stored.payload["schemaVersion"], json!(1));
    // Every §4.1 key is present, nothing else.
    let mut keys: Vec<&String> = stored.payload.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "activity_at_ms",
            "attention",
            "cards",
            "items",
            "schemaVersion",
            "working"
        ]
    );
}

/// The wake-up table (§4.3): which events resolve to which track.
#[tokio::test]
async fn wakeup_events_resolve_to_their_track() {
    let f = fx().await;
    let t = f.track("w").await;
    let card = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let mut rx = f.events.subscribe();
    // A card-status overlay.set with a degraded (System) scope resolves via
    // the card.
    f.spawn_fsm();
    f.claude_hook(&t, &card, ("PreToolUse", "pre_tool_use"), json!({}))
        .await;
    let mut seen_overlay = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while seen_overlay.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no overlay.set arrived"
        );
        let env = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::OverlaySet(o) = &env.event
            && o.entity_kind == "card"
            && o.kind == "status"
        {
            seen_overlay = Some(env);
        }
    }
    let mut env = seen_overlay.unwrap();
    assert_eq!(
        f.projector.track_for_event(&env).await.as_deref(),
        Some(t.as_str())
    );
    env.scope = EventScope::System;
    assert_eq!(
        f.projector.track_for_event(&env).await.as_deref(),
        Some(t.as_str()),
        "System scope falls back to card_get"
    );
    // The projector's own activity row is not a wake-up (no loop).
    let own = calm_server::event::BroadcastEnvelope {
        id: 0,
        event_version: 0,
        actor: ActorId::Kernel,
        scope: EventScope::System,
        event: Event::OverlaySet(calm_server::model::Overlay {
            id: "o".into(),
            plugin_id: "kernel".into(),
            entity_kind: "track".into(),
            entity_id: t.clone(),
            kind: "activity".into(),
            payload: json!({}),
            updated_at: 0,
        }),
    };
    assert_eq!(f.projector.track_for_event(&own).await, None);
}
