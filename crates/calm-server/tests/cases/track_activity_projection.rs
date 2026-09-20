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
//!   `task_recovery_preparation.rs` does — the driver is the only writer;
//! * transcript rows (E1/E2): the outcome writer `harness_turn_outcome_put`
//!   (`turn/completed`) and the item writer the run loop's `insert_item_row`
//!   calls (the only `item/*` writer); both stamp the clock, so a fixture
//!   UPDATE pins each row to a known instant afterwards (S1b's
//!   `track_conversations.rs` does the same).

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, card_create_with_id_tx, overlay_upsert_tx,
    session_start_runtime_tx, session_supersede_and_start_tx, task_claim_pending_tx,
    task_complete_from_worker_tx, task_fail_from_worker_tx, task_mark_running_tx,
    task_mark_sub_track_running_tx, task_recovery_allocate_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, EventScope, SYNC_EVENT_VERSION,
    TrackUpdatedPayload,
};
use calm_server::harness::{
    HarnessConfig, HarnessRegistry, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{
    CardRole, NewArea, NewCard, NewOverlay, NewTrack, Overlay, RequestTheme, TrackLifecycle,
    TrackPatch, now_ms,
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
use calm_server::track_activity::sql::{
    E1_HARNESS_TURN_COMPLETED_SQL, E2_USER_NOTIFY_SQL, SessionRow,
};
use calm_server::track_activity::{
    ActivityPayload, Attention, CardActivity, CardState, ItemKind, ItemSource, Recompute,
    TrackActivityProjector, WriteOutcome, fold,
};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_lifecycle::{
    apply_requested_transition_in_tx, auto_transition_if_current_in_tx,
};
use calm_server::track_report::{persist_report, resolve_report_for_track, tasks_rebuild_tx};
use calm_truth::validation::OVERLAY_KIND_REGISTRY;
use calm_types::harness::HarnessPhaseTag;
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

    /// #1743 S1 — archive through the same `track_update_tx` UPDATE the
    /// lifecycle rides on (K20: `lifecycle, terminal_at, archived_at,
    /// updated_at` are one statement).
    async fn archive(&self, track_id: &str, at_ms: i64) {
        self.repo_dyn
            .track_update(
                track_id,
                TrackPatch {
                    archived_at: Some(Some(at_ms)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    /// #1743 S1 — the planner's last completed turn (P, design §4.1) as a
    /// direct column write. The feeder's own writer
    /// (`session_record_activity_by_thread`) refuses a superseded row, and
    /// `superseded_planner_turn_still_ages` needs the value on exactly such
    /// a row (written while it was live, then superseded), so the fixture
    /// writes the column the feeder writes.
    async fn planner_turn_completed(&self, session_id: &str, at_ms: Option<i64>) {
        sqlx::query("UPDATE worker_sessions SET last_turn_completed_ms = ?1 WHERE id = ?2")
            .bind(at_ms)
            .bind(session_id)
            .execute(&self.pool)
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

    /// Bounded wait for the stored row to satisfy `pred` (the loop test's
    /// only clock: 3 s, far inside the 30 s tick).
    async fn await_stored(
        &self,
        track_id: &str,
        what: &str,
        pred: impl Fn(&ActivityPayload) -> bool,
    ) -> ActivityPayload {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let stored = self.stored(track_id).await;
            if let Some(p) = stored.as_ref().filter(|p| pred(p)) {
                return p.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what} on {track_id}: {stored:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// One `turn/completed` transcript row through the outcome writer
    /// (`harness::turn_outcome::record` → `harness_turn_outcome_put`);
    /// `turn` is the turn object, `status` at its root. Returns the row id.
    async fn turn_outcome(
        &self,
        ws: &str,
        card: &str,
        track: &str,
        turn_id: &str,
        turn: Value,
    ) -> i64 {
        self.repo_dyn
            .harness_turn_outcome_put(ws, card, track, "th-fixture", turn_id, &turn.to_string())
            .await
            .unwrap()
    }

    /// One `item/*` transcript row through the item writer (the run loop's
    /// `insert_item_row`). Returns the row id.
    #[allow(clippy::too_many_arguments)]
    async fn transcript_item(
        &self,
        ws: &str,
        card: &str,
        track: &str,
        item_uuid: &str,
        item_type: &str,
        method: &str,
        params: Value,
    ) -> i64 {
        self.repo_dyn
            .harness_item_insert(
                ws,
                card,
                track,
                "th-fixture",
                Some("turn-fixture"),
                Some(item_uuid),
                Some(item_type),
                method,
                &params.to_string(),
                None,
            )
            .await
            .unwrap()
    }

    /// Pin a transcript row to a known instant AFTER the production writer
    /// stamped the clock (the writers take no time argument).
    async fn pin_transcript_row(&self, id: i64, at_ms: i64) {
        sqlx::query("UPDATE harness_items SET created_at_ms = ?1 WHERE id = ?2")
            .bind(at_ms)
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// A `harness.item.added` event as the run loop's `emit_item_added`
    /// shapes it.
    fn item_added(track: &str, card: &str, method: &str, item_type: Option<&str>) -> Event {
        Event::HarnessItemAdded {
            worker_session_id: "ws-fixture".into(),
            card_id: CardId::from(card.to_string()),
            track_id: TrackId::from(track.to_string()),
            item_db_id: 1,
            item_uuid: None,
            item_type: item_type.map(str::to_string),
            turn_id: None,
            method: method.into(),
        }
    }

    fn track_scope(&self, track_id: &str) -> EventScope {
        EventScope::Track {
            track: TrackId::from(track_id.to_string()),
            area: AreaId::from(self.area_id.clone()),
        }
    }

    fn envelope(scope: EventScope, event: Event) -> BroadcastEnvelope {
        BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::Kernel,
            scope,
            event,
        }
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
// #1743 S1 — terminal-phase filter and failure aging (design §4.1)
// ---------------------------------------------------------------------------

/// A worker card with a `failed` current attempt (`calm.task.fail` at
/// `at_ms`) whose session is still `running`. Returns `(card, session)`.
async fn failed_attempt(
    f: &Fx,
    t: &str,
    card: &str,
    ws: &str,
    key: &str,
    at_ms: i64,
) -> (String, String) {
    let worker = f.card(t, card, "codex", CardRole::Worker).await;
    let session = f
        .session(
            &worker,
            ws,
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some(&format!("th-{ws}")),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(t, &[(key, "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(t, key, 2_000).await;
    f.mark_running(t, key, &worker, 3_000).await;
    f.fail(t, key, &worker, at_ms).await;
    (worker, session)
}

/// A planner card on `t` with an `idle` harness session (`cards.role =
/// 'planner'`, `last_turn_completed_ms` NULL — the mint value).
async fn planner_on(f: &Fx, t: &str, card: &str, ws: &str) -> (String, String) {
    let planner = f.card(t, card, "planner", CardRole::Planner).await;
    let session = f
        .session(
            &planner,
            ws,
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some(&format!("th-{ws}")),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    (planner, session)
}

/// Rule 1: a `done` track has nothing waiting on a person — the failed
/// attempt (task item + the same failure's `session` item) and the card
/// verdict all go; the E3 high-water mark stays (rule 3).
#[tokio::test]
async fn done_track_failed_attempt_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let (worker, ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 4_500).await;
    let before = f.recompute(&t).await;
    assert_eq!(before.attention, Attention::Failed, "{before:?}");
    assert_eq!(before.items.len(), 2, "task + session items: {before:?}");
    assert_eq!(card_state(&before, &worker), Some(CardState::Failed));

    f.set_lifecycle(&t, TrackLifecycle::Done).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "done ⇒ none: {p:?}");
    assert!(p.items.is_empty(), "{p:?}");
    assert!(
        p.cards.is_empty(),
        "the failed verdict goes with its item: {p:?}"
    );
    assert!(!p.working);
    assert_eq!(p.activity_at_ms, Some(4_000), "E3 is not filtered");
    let stored = f.stored(&t).await.unwrap();
    assert_eq!(stored.attention, Attention::None);
}

/// Rule 1, the archived half: `archived_at IS NOT NULL` filters the same
/// way whatever the lifecycle says (the tick no longer enumerates the
/// track, but an event-driven recompute still reaches it).
#[tokio::test]
async fn archived_track_failed_attempt_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    let before = f.recompute(&t).await;
    assert_eq!(before.attention, Attention::Failed, "{before:?}");
    assert_eq!(card_state(&before, &worker), Some(CardState::Failed));

    f.archive(&t, 5_000).await;
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tracks WHERE id = ?1")
        .bind(&t)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        lifecycle, "working",
        "archiving does not touch the lifecycle"
    );
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "archived ⇒ none: {p:?}");
    assert!(p.items.is_empty(), "{p:?}");
    assert!(p.cards.is_empty(), "{p:?}");
    assert!(!p.working);
}

/// The twin of rule 1: `done → planning` through `track_update_tx` (the
/// user reopening) brings the failed attempt back — the filter is a
/// function of the row, not a one-way write.
#[tokio::test]
async fn reopened_track_failed_attempt_is_red_again() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    f.set_lifecycle(&t, TrackLifecycle::Done).await;
    let quiet_now = f.recompute(&t).await;
    assert_eq!(quiet_now.attention, Attention::None, "{quiet_now:?}");
    assert!(quiet_now.items.is_empty());

    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let terminal_at: Option<i64> =
        sqlx::query_scalar("SELECT terminal_at FROM tracks WHERE id = ?1")
            .bind(&t)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(terminal_at, None, "reopening clears terminal_at (K20)");
    let p = f.recompute(&t).await;
    assert_eq!(
        p.attention,
        Attention::Failed,
        "reopened ⇒ red again: {p:?}"
    );
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Task);
    assert_eq!(p.items[0].id, "build");
    assert_eq!(p.items[0].at_ms, 4_000);
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
}

/// Rule 1 filters `items` / `attention` / the `input`+`failed` card
/// verdicts ONLY: a task still `running` on a done track keeps `working`
/// and its `cards[] = working` entry (S2 is what ends it, not the fold).
/// The same-card variant (review r1, A MINOR-1 / codex P2): a card whose
/// `failed` verdict out-ranked its `working` one (`build` failed and `test`
/// running on the SAME worker) keeps the working verdict too — the filter
/// removes the attention verdict, not the card.
#[tokio::test]
async fn done_track_running_task_is_still_working() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let failed_worker = f.card(&t, "card-a", "codex", CardRole::Worker).await;
    let running_worker = f.card(&t, "card-b", "codex", CardRole::Worker).await;
    let both_worker = f.card(&t, "card-x", "codex", CardRole::Worker).await;
    for (card, ws, th) in [
        (&failed_worker, "ws-a", "th-a"),
        (&running_worker, "ws-b", "th-b"),
        (&both_worker, "ws-x", "th-x"),
    ] {
        f.session(
            card,
            ws,
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some(th),
            None,
            1_000,
        )
        .await;
    }
    f.plan_tasks(
        &t,
        &[
            ("build", "codex", TASK_IN_TRACK_ROUTE, None),
            ("test", "codex", TASK_IN_TRACK_ROUTE, None),
            ("lint", "codex", TASK_IN_TRACK_ROUTE, None),
            ("pack", "codex", TASK_IN_TRACK_ROUTE, None),
        ],
    )
    .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &failed_worker, 3_000).await;
    f.fail(&t, "build", &failed_worker, 4_000).await;
    f.claim(&t, "test", 5_000).await;
    f.mark_running(&t, "test", &running_worker, 6_000).await;
    // The same-card pair: `lint` failed (X, 4000) and `pack` running (X, 6000).
    f.claim(&t, "lint", 2_000).await;
    f.mark_running(&t, "lint", &both_worker, 3_000).await;
    f.fail(&t, "lint", &both_worker, 4_000).await;
    f.claim(&t, "pack", 5_000).await;
    f.mark_running(&t, "pack", &both_worker, 6_000).await;
    let before = f.recompute(&t).await;
    assert!(before.working, "{before:?}");
    assert_eq!(before.attention, Attention::Failed);
    assert_eq!(card_state(&before, &failed_worker), Some(CardState::Failed));
    assert_eq!(
        card_state(&before, &running_worker),
        Some(CardState::Working)
    );
    assert_eq!(
        card_state(&before, &both_worker),
        Some(CardState::Failed),
        "failed > working while the failure counts: {before:?}"
    );

    f.set_lifecycle(&t, TrackLifecycle::Done).await;
    let p = f.recompute(&t).await;
    assert!(
        p.working,
        "a running task on a done track is not hidden: {p:?}"
    );
    assert_eq!(p.attention, Attention::None);
    assert!(p.items.is_empty());
    assert_eq!(
        p.cards,
        vec![
            CardActivity {
                card_id: running_worker.clone(),
                state: CardState::Working
            },
            CardActivity {
                card_id: both_worker.clone(),
                state: CardState::Working
            },
        ],
        "every card with working evidence keeps it, the failed-only card goes: {p:?}"
    );
}

/// Rule 1, the `input` form of the same-card corner (codex P2's
/// construction): a done track, a task running on X, and X's codex session
/// `waitingOnApproval` — the input verdict out-ranked the working one; the
/// filter drops the input, `cards == [X = working]`, `attention = none`.
#[tokio::test]
async fn done_track_input_on_a_running_worker_is_still_working() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Working).await;
    let worker = f.card(&t, "card-x", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-x",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-x"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.stamp("th-x", 4_000, "waitingOnApproval", None).await;
    let before = f.recompute(&t).await;
    assert!(before.working, "{before:?}");
    assert_eq!(before.attention, Attention::Input);
    assert_eq!(
        card_state(&before, &worker),
        Some(CardState::Input),
        "input > working on a live track: {before:?}"
    );

    f.set_lifecycle(&t, TrackLifecycle::Done).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(p.attention, Attention::None);
    assert!(p.items.is_empty());
    assert_eq!(
        p.cards,
        vec![CardActivity {
            card_id: worker.clone(),
            state: CardState::Working
        }],
        "the working evidence outlives the filtered input verdict: {p:?}"
    );
}

/// Rule 2, the counted side: a failure newer than the planner's last
/// completed turn (`at_ms > P`) is still red.
#[tokio::test]
async fn failed_after_planner_turn_is_red() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let (_planner, planner_ws) = planner_on(&f, &t, "card-planner", "ws-planner").await;
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    f.planner_turn_completed(&planner_ws, Some(3_000)).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed, "4000 > P=3000 ⇒ red: {p:?}");
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Task);
    assert_eq!(p.items[0].at_ms, 4_000);
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
    assert!(!p.working);
}

/// Rule 2, the aged side: a failure at or before the planner's last
/// completed turn (`at_ms <= P`) has been handled — no item, no card
/// verdict, `attention = none`; its `finished_at_ms` still feeds E3.
#[tokio::test]
async fn failed_before_planner_turn_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let (_planner, planner_ws) = planner_on(&f, &t, "card-planner", "ws-planner").await;
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    let red = f.recompute(&t).await;
    assert_eq!(red.attention, Attention::Failed, "no turn yet: {red:?}");

    f.planner_turn_completed(&planner_ws, Some(5_000)).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "4000 <= P=5000 ⇒ aged: {p:?}");
    assert!(p.items.is_empty(), "{p:?}");
    assert!(
        p.cards.is_empty(),
        "the aged failure's card verdict goes too: {p:?}"
    );
    assert!(!p.working);
    assert_eq!(
        p.activity_at_ms,
        Some(4_000),
        "E3 still counts the aged failure"
    );
    assert_eq!(card_state(&p, &worker), None);

    // The boundary is `>`: a turn completed AT the failure instant ages it.
    f.planner_turn_completed(&planner_ws, Some(4_000)).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "4000 <= P=4000 ⇒ aged: {p:?}");
    assert!(p.items.is_empty());
}

/// Rule 2 with P NULL: a planner that has never completed a turn has
/// handled nothing — the failure stays red (the `map_or(true, …)` arm).
#[tokio::test]
async fn null_planner_turn_keeps_failed_red() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let (_planner, planner_ws) = planner_on(&f, &t, "card-planner", "ws-planner").await;
    let p_column: Option<i64> =
        sqlx::query_scalar("SELECT last_turn_completed_ms FROM worker_sessions WHERE id = ?1")
            .bind(&planner_ws)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(p_column, None, "the mint value is NULL");
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed, "P NULL ⇒ counted: {p:?}");
    assert_eq!(p.items.len(), 1);
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
}

/// P is the max over EVERY planner-role session of the track, superseded
/// ones included: a planner restart (new session, column NULL) does not
/// bring an already-handled failure back.
#[tokio::test]
async fn superseded_planner_turn_still_ages() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let (planner, old_ws) = planner_on(&f, &t, "card-planner", "ws-planner-1").await;
    // The old session completed a turn at 5000 while live…
    f.planner_turn_completed(&old_ws, Some(5_000)).await;
    // …then the planner restarted: the production supersede-and-start
    // writer parks it as `superseded` and mints the current session
    // (column NULL, `cards.session_id` repointed).
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &old_ws,
        WorkerSessionInit {
            id: "ws-planner-2".into(),
            card_id: planner.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("th-planner-2".into()),
            session_id: Some("native-ws-planner-2".into()),
            active_turn_id: None,
            handle_state_json: Some(Fx::harness_snapshot()),
            spawn_op_id: None,
            now_ms: 6_000,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (old_state, current): (String, Option<String>) = sqlx::query_as(
        "SELECT ws.state, c.session_id FROM worker_sessions ws JOIN cards c ON c.id = ws.card_id \
          WHERE ws.id = ?1",
    )
    .bind(&old_ws)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(old_state, "superseded");
    assert_eq!(current.as_deref(), Some("ws-planner-2"));

    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(
        p.attention,
        Attention::None,
        "the superseded planner's P=5000 ages the 4000 failure: {p:?}"
    );
    assert!(p.items.is_empty(), "{p:?}");
    assert_eq!(card_state(&p, &worker), None);
}

/// Rule 2 ages `task` / `session` failures only: the track's own
/// `lifecycle = failed` item is a phase, and a later planner turn does not
/// age it.
#[tokio::test]
async fn lifecycle_failed_item_is_not_aged() {
    let f = fx().await;
    let t = f.track("w").await;
    let (_planner, planner_ws) = planner_on(&f, &t, "card-planner", "ws-planner").await;
    f.set_lifecycle(&t, TrackLifecycle::Failed).await;
    let updated_at: i64 = sqlx::query_scalar("SELECT updated_at FROM tracks WHERE id = ?1")
        .bind(&t)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    f.planner_turn_completed(&planner_ws, Some(updated_at + 1_000))
        .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::Failed, "a phase is not aged: {p:?}");
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].source, ItemSource::Lifecycle);
    assert_eq!(p.items[0].kind, ItemKind::Failed);
    assert_eq!(p.items[0].id, t);
    assert_eq!(p.items[0].at_ms, updated_at);
}

/// An aged failure takes its `cards[card] = failed` with it: the same
/// worker card's running attempt then folds to `working`, not `failed`
/// (`failed > working` would otherwise win).
#[tokio::test]
async fn aged_failure_drops_its_card_verdict() {
    let f = fx().await;
    let t = f.track("w").await;
    f.set_lifecycle(&t, TrackLifecycle::Planning).await;
    let (_planner, planner_ws) = planner_on(&f, &t, "card-planner", "ws-planner").await;
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
    f.mark_running(&t, "test", &worker, 6_000).await;
    let before = f.recompute(&t).await;
    assert!(before.working);
    assert_eq!(before.attention, Attention::Failed);
    assert_eq!(
        card_state(&before, &worker),
        Some(CardState::Failed),
        "failed > working while the failure counts: {before:?}"
    );

    f.planner_turn_completed(&planner_ws, Some(4_500)).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(p.attention, Attention::None);
    assert!(p.items.is_empty());
    assert_eq!(
        p.cards,
        vec![CardActivity {
            card_id: worker.clone(),
            state: CardState::Working
        }],
        "the aged failure no longer raises the card: {p:?}"
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

/// E1, row-level: the `turn/completed` transcript row whose `status` is
/// `completed` is the activity instant (its `created_at_ms`); an
/// `interrupted` twin written LATER does not move it; a `failed` turn is an
/// ending (S1b's predicate excludes only `interrupted`).
#[tokio::test]
async fn e1_turn_completed_row_is_the_activity_instant() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    assert_eq!(f.recompute(&t).await.activity_at_ms, None, "no row yet");

    let t1 = 1_700_000_000_000_i64;
    let done = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-1",
            json!({"id": "turn-1", "status": "completed"}),
        )
        .await;
    f.pin_transcript_row(done, t1).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1),
        "E1 = the completed row's created_at_ms"
    );

    let interrupted = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-2",
            json!({"id": "turn-2", "status": "interrupted"}),
        )
        .await;
    f.pin_transcript_row(interrupted, t1 + 5_000).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1),
        "a later interrupted turn is not an ending"
    );

    let failed = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-3",
            json!({"id": "turn-3", "status": "failed"}),
        )
        .await;
    f.pin_transcript_row(failed, t1 + 7_000).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1 + 7_000),
        "a failed turn is an ending"
    );
}

/// E2, row-level: the `item/completed` `mcpToolCall` row of a successful
/// `calm.user.notify` is the activity instant; twins with `item.error`
/// set, `item.status = 'failed'`, the `item/started` half of the same call,
/// or another tool do not move it.
#[tokio::test]
async fn e2_user_notify_completed_row_is_the_activity_instant() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    let notify = |status: &str, error: Option<&str>| {
        let mut item = json!({
            "id": "call-1", "type": "mcpToolCall", "server": "calm",
            "tool": "calm.user.notify", "status": status,
        });
        if let Some(e) = error {
            item["error"] = json!({"message": e});
        }
        json!({"threadId": "th-fixture", "turnId": "turn-fixture", "item": item})
    };

    let t2 = 1_700_000_100_000_i64;
    let ok = f
        .transcript_item(
            &ws,
            &planner,
            &t,
            "call-1",
            "mcpToolCall",
            "item/completed",
            notify("completed", None),
        )
        .await;
    f.pin_transcript_row(ok, t2).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t2),
        "E2 = the completed notify row's created_at_ms"
    );

    let twins: [(&str, &str, Value); 4] = [
        (
            "call-err",
            "item/completed",
            notify("completed", Some("boom")),
        ),
        ("call-failed", "item/completed", notify("failed", None)),
        ("call-started", "item/started", notify("inProgress", None)),
        (
            "call-other",
            "item/completed",
            json!({"item": {"id": "call-other", "type": "mcpToolCall", "server": "calm",
                            "tool": "calm.task.complete", "status": "completed"}}),
        ),
    ];
    for (i, (uuid, method, params)) in twins.into_iter().enumerate() {
        let id = f
            .transcript_item(&ws, &planner, &t, uuid, "mcpToolCall", method, params)
            .await;
        f.pin_transcript_row(id, t2 + 5_000 * (i as i64 + 1)).await;
        assert_eq!(
            f.recompute(&t).await.activity_at_ms,
            Some(t2),
            "{uuid} ({method}) is not evidence"
        );
    }
}

/// The production E1/E2 statements (the `pub const`s the projector runs)
/// enter the transcript table through `idx_transcript_card_method_created_at`
/// — one index range per card, no scan (design §4.3, F2.35). S1b's plan
/// test in `calm-truth` pins an E1-SHAPED statement; this one pins the
/// text the projector actually executes.
#[tokio::test]
async fn e1_e2_query_plans_use_the_transcript_index() {
    let f = fx().await;
    const INDEX: &str = "USING INDEX idx_transcript_card_method_created_at";
    for (label, sql) in [
        ("E1", E1_HARNESS_TURN_COMPLETED_SQL),
        ("E2", E2_USER_NOTIFY_SQL),
    ] {
        let details: Vec<String> = sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
            .bind("track-1")
            .fetch_all(&f.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| sqlx::Row::get::<String, _>(&row, "detail"))
            .collect();
        assert!(
            details.iter().any(|d| d.contains(INDEX)),
            "{label} must be an index range per card, got plan {details:?}"
        );
        assert!(
            !details.iter().any(|d| d.starts_with("SCAN")),
            "{label} must not scan any table, got plan {details:?}"
        );
    }
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

/// M10 across versions: a stored payload THIS binary cannot parse (an
/// `attention` value it does not know, plus a key it does not know — what a
/// newer binary leaves behind) still keeps its high-water mark; the
/// conclusions are recomputed and the row is rewritten in this shape. The
/// mark is read from the raw JSON, not from the parsed struct — otherwise a
/// downgrade would re-seed it and light a spurious unread.
#[tokio::test]
async fn high_water_mark_survives_an_unparseable_stored_payload() {
    let f = fx().await;
    let t = f.track("w").await;
    let big = 4_000_000_000_000_i64;
    f.seed_activity_overlay(
        &t,
        json!({
            "schemaVersion": 1, "working": true, "attention": "review",
            "activity_at_ms": big, "items": [], "cards": [], "reviewers": ["someone"]
        }),
    )
    .await;
    assert!(
        serde_json::from_value::<ActivityPayload>(
            f.repo_dyn.overlays_for("track", &t).await.unwrap()[0]
                .payload
                .clone()
        )
        .is_err(),
        "the seeded row must NOT parse, or this test proves nothing"
    );
    let p = match f.projector.recompute_track(&t).await.unwrap() {
        Recompute::Written(p) => p,
        other => panic!("an unparseable row is rewritten in this binary's shape: {other:?}"),
    };
    assert_eq!(p.activity_at_ms, Some(big), "{p:?}");
    let stored = f.stored(&t).await.unwrap();
    assert_eq!(stored.activity_at_ms, Some(big));
    assert!(
        !stored.working,
        "the conclusions come from the rows, not the old row"
    );
    assert_eq!(stored.attention, Attention::None);
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

/// Review r1 (Codex P2): the reads and the write share no snapshot. A track
/// deleted between them has already lost every overlay row in its delete
/// transaction (`routes/tracks.rs` / `Repo::track_delete`); the late write
/// must not put an orphan `activity` row back (no FK; the reconcile
/// enumerates live tracks only, so it would be permanent) and must emit
/// nothing. The write half runs with the payload the read half computed
/// BEFORE the delete — the exact race.
#[tokio::test]
async fn deleted_track_is_not_resurrected_by_a_late_write() {
    let f = fx().await;
    let t = f.track("w").await;
    let _worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    // The read half, before the delete.
    let rows = f
        .projector
        .read_rows(&t)
        .await
        .unwrap()
        .expect("the track exists at read time");
    let folded = fold(&t, &rows);
    let late = ActivityPayload {
        schema_version: 1,
        working: folded.working,
        attention: folded.attention(),
        activity_at_ms: None,
        items: folded.items,
        cards: folded.cards,
    };
    assert!(late.working, "the late write would say working: {late:?}");
    // The delete lands: card overlays, track overlays, sessions, tasks,
    // the track row — one transaction.
    f.repo_dyn.track_delete(&t).await.unwrap();
    let mut rx = f.events.subscribe();
    // The write half, with the pre-delete payload.
    assert_eq!(
        f.projector.write_overlay(&t, &late).await.unwrap(),
        WriteOutcome::TrackGone
    );
    let orphans: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM overlays WHERE entity_kind = 'track' AND entity_id = ?1",
    )
    .bind(&t)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(orphans, 0, "no overlay row for a deleted track");
    assert!(rx.try_recv().is_err(), "no overlay.set was broadcast");
    // A full recomputation of the deleted track is a no-op as well.
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::NoTrack
    ));
    assert!(rx.try_recv().is_err());
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

/// The wake-up table (design §4.3), every row, table-driven: which events
/// resolve to which track, and the ones that must not wake anything
/// (`track.deleted`; the projector's own row; the `item/started` half of a
/// tool call, review r1 A-MIN4; a card no row knows).
#[tokio::test]
async fn wakeup_table_resolves_every_row_of_the_design() {
    let f = fx().await;
    let t = f.track("w").await;
    let card = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let track = f.repo_dyn.track_get(&t).await.unwrap().unwrap();
    let tid = TrackId::from(t.clone());
    let cid = CardId::from(card.clone());
    let area = AreaId::from(f.area_id.clone());
    let overlay = |plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str| {
        Event::OverlaySet(Overlay {
            id: "o".into(),
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload: json!({}),
            updated_at: 0,
        })
    };
    let task_events = [
        (
            "task.dispatched",
            Event::TaskDispatched {
                idempotency_key: format!("{t}:build"),
                kind: "codex".into(),
                agent_message: None,
            },
        ),
        (
            "task.completed",
            Event::TaskCompleted {
                idempotency_key: format!("{t}:build"),
                result: json!({}),
                artifacts: vec![],
                agent_message: None,
            },
        ),
        (
            "task.failed",
            Event::TaskFailed {
                idempotency_key: format!("{t}:build"),
                reason: "fixture".into(),
                details: None,
                agent_message: None,
            },
        ),
        (
            "task.execution_settled",
            Event::TaskExecutionSettled {
                task_id: format!("{t}:build"),
                operation_id: "op".into(),
            },
        ),
        (
            "task.gate_result",
            Event::TaskGateResult {
                task_id: format!("{t}:build"),
                idempotency_key: format!("{t}:build#g1"),
                passed: true,
                failing_step: None,
                exit_code: Some(0),
                log_tail: String::new(),
                log_path: String::new(),
                attempt: 1,
                agent_message: None,
            },
        ),
    ];
    let session_events = [
        (
            "worker_session.started",
            Event::WorkerSessionStarted {
                worker_session_id: "ws-w".into(),
                card_id: card.clone(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
            },
        ),
        (
            "worker_session.status_changed",
            Event::WorkerSessionStatusChanged {
                worker_session_id: "ws-w".into(),
                card_id: card.clone(),
                old_status: WorkerSessionState::Starting,
                new_status: WorkerSessionState::Running,
            },
        ),
        (
            "worker_session.superseded",
            Event::WorkerSessionSuperseded {
                old_worker_session_id: "ws-w".into(),
                new_worker_session_id: "ws-w2".into(),
                card_id: card.clone(),
            },
        ),
    ];

    let mut rows: Vec<(String, EventScope, Event, Option<&str>)> = vec![
        (
            "overlay.set kernel/card/status, track scope".into(),
            f.track_scope(&t),
            overlay("kernel", "card", &card, "status"),
            Some(t.as_str()),
        ),
        (
            "overlay.set kernel/card/status, System scope → card_get".into(),
            EventScope::System,
            overlay("kernel", "card", &card, "status"),
            Some(t.as_str()),
        ),
        (
            "overlay.set kernel/card/status of a card no row knows".into(),
            EventScope::System,
            overlay("kernel", "card", "no-such-card", "status"),
            None,
        ),
        (
            "overlay.set kernel/track/activity — the projector's own row".into(),
            f.track_scope(&t),
            overlay("kernel", "track", &t, "activity"),
            None,
        ),
        (
            "overlay.set of a plugin, card/status".into(),
            f.track_scope(&t),
            overlay("plugin-x", "card", &card, "status"),
            None,
        ),
        (
            "harness.phase.changed".into(),
            EventScope::System,
            Event::HarnessPhaseChanged {
                worker_session_id: "ws-w".into(),
                card_id: cid.clone(),
                track_id: tid.clone(),
                old_phase: HarnessPhaseTag::TurnRunning,
                new_phase: HarnessPhaseTag::Idle,
            },
            Some(t.as_str()),
        ),
        (
            "harness.item.added mcpToolCall item/completed".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", Some("mcpToolCall")),
            Some(t.as_str()),
        ),
        (
            "harness.item.added mcpToolCall item/started (not a completion)".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/started", Some("mcpToolCall")),
            None,
        ),
        (
            "harness.item.added agentMessage item/completed".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", Some("agentMessage")),
            None,
        ),
        (
            "harness.item.added without an item type".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", None),
            None,
        ),
        (
            "track.lifecycle_changed".into(),
            EventScope::System,
            Event::TrackLifecycleChanged {
                id: tid.clone(),
                area_id: area.clone(),
                from: TrackLifecycle::Draft,
                to: TrackLifecycle::Planning,
                agent_message: None,
            },
            Some(t.as_str()),
        ),
        (
            "track.report_edited".into(),
            EventScope::System,
            Event::TrackReportEdited {
                track_id: tid.clone(),
                card_id: cid.clone(),
                author: EditAuthor::Planner,
                author_plugin_id: None,
                edit_id: "e1".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: String::new(),
                agent_message: None,
            },
            Some(t.as_str()),
        ),
        (
            "track.updated".into(),
            EventScope::System,
            Event::TrackUpdated(TrackUpdatedPayload::new(track, None)),
            Some(t.as_str()),
        ),
        (
            "track.deleted".into(),
            f.track_scope(&t),
            Event::TrackDeleted {
                id: tid.clone(),
                area_id: area.clone(),
            },
            None,
        ),
    ];
    for (label, event) in session_events {
        rows.push((
            label.into(),
            EventScope::System,
            event.clone(),
            Some(t.as_str()),
        ));
        let Event::WorkerSessionStarted { .. } = &event else {
            continue;
        };
        rows.push((
            format!("{label} of a card no row knows"),
            EventScope::System,
            Event::WorkerSessionStarted {
                worker_session_id: "ws-x".into(),
                card_id: "no-such-card".into(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
            },
            None,
        ));
    }
    for (label, event) in task_events {
        rows.push((
            format!("{label}, track scope"),
            f.track_scope(&t),
            event.clone(),
            Some(t.as_str()),
        ));
        rows.push((
            format!("{label}, System scope (no track to wake)"),
            EventScope::System,
            event,
            None,
        ));
    }
    assert!(
        rows.len() >= 27,
        "every §4.3 row plus its negatives: {}",
        rows.len()
    );
    for (label, scope, event, expected) in rows {
        let env = Fx::envelope(scope, event);
        assert_eq!(
            f.projector.track_for_event(&env).await.as_deref(),
            expected,
            "{label}"
        );
    }
}

/// The loop end to end: `run()` (boot sweep + bus wake-ups + tick) is
/// spawned, a `task.dispatched` envelope arrives on the bus, and the
/// overlay flips to `working` within a bounded wait — far inside the 30 s
/// tick, so only the event path can have done it.
#[tokio::test]
async fn projector_loop_recomputes_on_task_dispatched() {
    let f = fx().await;
    let t = f.track("w").await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    let looped = TrackActivityProjector::new(
        f.repo_dyn.clone(),
        f.events.clone(),
        f.write.clone(),
        f.harness.clone(),
    )
    .expect("sqlite-backed repo");
    let loop_task = tokio::spawn(looped.run());
    // The boot sweep (the interval's first tick completes immediately)
    // seeds a quiet row.
    let seeded = f.await_stored(&t, "the boot sweep's row", |_| true).await;
    assert!(quiet(&seeded), "{seeded:?}");

    // A silent claim (the fixture's claim appends no event), then the
    // wake-up the scheduler's claim transaction would have carried.
    f.claim(&t, "build", 2_000).await;
    assert!(
        !f.stored(&t).await.unwrap().working,
        "nothing woke the loop yet"
    );
    f.events.emit_envelope_for_test(Fx::envelope(
        f.track_scope(&t),
        Event::TaskDispatched {
            idempotency_key: format!("{t}:build"),
            kind: "codex".into(),
            agent_message: None,
        },
    ));
    let p = f
        .await_stored(&t, "working after task.dispatched", |p| p.working)
        .await;
    assert_eq!(p.attention, Attention::None);
    loop_task.abort();
}
