//! The in-memory repo, event bus and `kernel/track/activity` projector rig shared by the
//! track-activity tests: `Fx` drives the production writers of every row the projector reads.

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, card_create_with_id_tx, overlay_upsert_tx,
    session_start_runtime_tx, task_claim_pending_tx, task_complete_from_worker_tx,
    task_fail_from_worker_tx, task_mark_running_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, EventScope, SYNC_EVENT_VERSION,
};
use calm_server::harness::{
    HarnessConfig, HarnessRegistry, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{
    CardRole, NewArea, NewCard, NewOverlay, NewTrack, RequestTheme, TrackPatch, now_ms,
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
use calm_server::track_activity::{ActivityPayload, Recompute, TrackActivityProjector};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::{persist_report, resolve_report_for_track, tasks_rebuild_tx};
use calm_types::report_blocks::render_fence;
use calm_types::task_recovery::{TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint};
use calm_types::track_report::TrackReportPayload;
use calm_types::worker::WorkerSessionId;
use serde_json::{Value, json};

pub(crate) struct Fx {
    repo: Arc<SqlxRepo>,
    pub(crate) repo_dyn: Arc<dyn Repo>,
    pub(crate) pool: sqlx::SqlitePool,
    pub(crate) events: EventBus,
    pub(crate) write: WriteContext,
    pub(crate) role_cache: CardRoleCache,
    pub(crate) area_cache: TrackAreaCache,
    pub(crate) harness: HarnessRegistry,
    pub(crate) projector: TrackActivityProjector,
    pub(crate) area_id: String,
}

pub(crate) async fn fx() -> Fx {
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
        TerminalRendererRegistry::new(),
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
    pub(crate) async fn track(&self, title: &str) -> String {
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

    pub(crate) async fn set_closed(&self, track_id: &str, closed: bool) {
        self.repo_dyn
            .track_update(
                track_id,
                TrackPatch {
                    closed: Some(closed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    pub(crate) async fn card(
        &self,
        track_id: &str,
        card_id: &str,
        kind: &str,
        role: CardRole,
    ) -> String {
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

    /// Mint a session through the production route; `handle_state_json = Some(harness snapshot)` makes it a harness row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn session(
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

    pub(crate) fn harness_snapshot() -> Value {
        serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap()
    }

    /// The exit writer: `state`, `liveness='exited'`, `completed_at_ms = updated_at_ms = at`.
    pub(crate) async fn exit_session(&self, session_id: &str, to: WorkerSessionState, at_ms: i64) {
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
    pub(crate) async fn stamp(&self, thread_id: &str, at_ms: i64, status: &str, done: Option<i64>) {
        self.repo_dyn
            .session_record_activity_by_thread(thread_id, at_ms, status, done)
            .await
            .unwrap();
    }

    /// Plan tasks the way the planner does; `decls` are `(key, kind, spawn, gate_json)`.
    pub(crate) async fn plan_tasks(
        &self,
        track_id: &str,
        decls: &[(&str, &str, &str, Option<Value>)],
    ) {
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
        )
        .await
        .unwrap();
    }

    pub(crate) fn task_id(track_id: &str, key: &str) -> String {
        format!("{track_id}:{key}")
    }

    /// The frozen report-block closure of a task.
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

    /// `pending → dispatched` (the scheduler's claim; `worker_card_id` stays NULL).
    pub(crate) async fn claim(&self, track_id: &str, key: &str, at_ms: i64) {
        self.claim_attempt(track_id, key, &Self::task_id(track_id, key), at_ms)
            .await;
    }

    pub(crate) async fn claim_attempt(
        &self,
        track_id: &str,
        key: &str,
        attempt_id: &str,
        at_ms: i64,
    ) {
        let refs = self.closure(track_id, key).await;
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        let n = task_claim_pending_tx(&mut tx, attempt_id, at_ms, &refs, false)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 1, "claim {key}");
    }

    /// `dispatched → running` + `worker_card_id` (the post-spawn stamp).
    pub(crate) async fn mark_running(
        &self,
        track_id: &str,
        key: &str,
        worker_card_id: &str,
        at_ms: i64,
    ) {
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

    /// The worker's `neige_task_done` flip (`done` + `finished_at_ms`).
    pub(crate) async fn complete(
        &self,
        track_id: &str,
        key: &str,
        worker_card_id: &str,
        at_ms: i64,
    ) {
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

    /// The worker's `neige_task_fail` flip (`failed` + `finished_at_ms`).
    pub(crate) async fn fail(&self, track_id: &str, key: &str, worker_card_id: &str, at_ms: i64) {
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

    /// A second attempt for a failed key, as a released recovery allocation (4140 keeps one; no code
    /// writes one any more) + projection rebuild. Returns the new attempt id.
    pub(crate) async fn recover(
        &self,
        track_id: &str,
        key: &str,
        previous_attempt_id: &str,
    ) -> String {
        let attempt_id = format!("{previous_attempt_id}:2");
        let origin = TaskAttemptOrigin::Recovery {
            previous_attempt_id: previous_attempt_id.to_string(),
            idempotency_key: format!("recover-{key}"),
            request_fingerprint: "fixture-fingerprint".into(),
            reason: "fixture recovery".into(),
            actor: ActorId::User,
            constraint: TaskRecoveryConstraint::V1 {
                refs: self.closure(track_id, key).await,
                spawn: TASK_IN_TRACK_ROUTE.into(),
                declared_by: "user".into(),
            },
        };
        let mut tx = begin_immediate_tx(&self.pool).await.unwrap();
        sqlx::query(
            "INSERT INTO task_attempt_allocations \
             (attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES (?1,?2,?3,2,?4,0)",
        )
        .bind(&attempt_id)
        .bind(track_id)
        .bind(key)
        .bind(serde_json::to_string(&origin).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
        tasks_rebuild_tx(&mut tx, track_id).await.unwrap();
        tx.commit().await.unwrap();
        attempt_id
    }

    pub(crate) async fn task_status(
        &self,
        attempt_id: &str,
    ) -> (String, Option<String>, Option<i64>) {
        sqlx::query_as("SELECT status, worker_card_id, finished_at_ms FROM tasks WHERE id = ?1")
            .bind(attempt_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Emit a hook exactly as `/internal/claude/hook` does; persisted as a `claude.hook` event row, nothing projects it.
    pub(crate) async fn claude_hook(
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

    /// Install a LIVE (unstarted) harness handle for `session_id` in the registry.
    pub(crate) async fn install_live_harness(
        &self,
        track_id: &str,
        card_id: &str,
        session_id: &str,
    ) {
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
                backend: daemon.into(),
                live_replies: calm_server::harness::LiveReplies::for_test(),
                config: HarnessConfig::default(),
                snapshot: HarnessSnapshot::initial(0, vec![]),
            },
            8,
        );
        self.harness.insert(session_id.to_string(), handle);
    }

    /// A scheduler over this repo, for `reconcile_child_track_task`.
    pub(crate) fn scheduler(&self) -> (Arc<OperationRuntime>, Arc<Scheduler>) {
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
            calm_server::per_card_lock::new_per_card_locks(),
            Arc::new(tokio::sync::Semaphore::new(4)),
            std::env::temp_dir().join("neige-test-gate-logs"),
            calm_server::scheduler::WorkerIdleWake::new(
                calm_server::shared_codex_appserver::SharedCodexAppServer::new_stub(
                    self.repo_dyn.clone(),
                ),
                calm_server::scheduler::WORKER_IDLE_TURN_GRACE,
                calm_server::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
            ),
        );
        (runtime, scheduler)
    }

    /// Seed a `kernel/track/activity` row directly (the projector's own writer shape).
    pub(crate) async fn seed_activity_overlay(&self, track_id: &str, payload: Value) {
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

    pub(crate) async fn recompute(&self, track_id: &str) -> ActivityPayload {
        match self.projector.recompute_track(track_id).await.unwrap() {
            Recompute::NoTrack => panic!("track {track_id} vanished"),
            Recompute::Unchanged(p) | Recompute::Written(p) => p,
        }
    }

    pub(crate) async fn stored(&self, track_id: &str) -> Option<ActivityPayload> {
        self.repo_dyn
            .overlays_for("track", track_id)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "activity" && o.plugin_id == "kernel")
            .map(|o| serde_json::from_value(o.payload).unwrap())
    }

    /// Bounded wait (3 s, far inside the 30 s tick) for the stored row to satisfy `pred`.
    pub(crate) async fn await_stored(
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

    /// One `turn/completed` transcript row through the outcome writer. Returns the row id.
    pub(crate) async fn turn_outcome(
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

    /// One `item/*` transcript row through the item writer. Returns the row id.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn transcript_item(
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

    /// The Planner card `card` (session `ws`) asks one question per title through the shared entry
    /// `neige_user_ask` writes with. Returns the ask's id.
    pub(crate) async fn ask(&self, card: &str, ws: &str, titles: &[&str]) -> i64 {
        let card = CardId::from(card.to_string());
        let actor = ActorId::AiPlannerSession(WorkerSessionId::from(ws));
        let questions: Vec<calm_server::event::AskQuestion> = titles
            .iter()
            .map(|title| calm_server::event::AskQuestion {
                title: (*title).to_string(),
                options: Vec::new(),
            })
            .collect();
        let (_, ids) = calm_server::db::write_with_actor_events_typed(
            self.repo_dyn.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let (scope, event) =
                        calm_server::ask::ask_requested_tx(tx, &card, questions).await?;
                    Ok(((), vec![(actor, scope, event)]))
                })
            },
        )
        .await
        .unwrap();
        ids[0]
    }

    /// The user answers `ask_id` on `track` through the shared entry the answer route writes with.
    pub(crate) async fn answer(&self, track: &str, ask_id: i64, answers: &[&str]) {
        let track = TrackId::from(track.to_string());
        let answers: Vec<calm_server::event::AskAnswer> = answers
            .iter()
            .map(|a| calm_server::event::AskAnswer::Text((*a).to_string()))
            .collect();
        calm_server::db::write_with_actor_events_typed(
            self.repo_dyn.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let (scope, events) =
                        calm_server::ask::ask_answered_tx(tx, &track, ask_id, answers).await?;
                    Ok((
                        (),
                        events
                            .into_iter()
                            .map(|event| (ActorId::User, scope.clone(), event))
                            .collect(),
                    ))
                })
            },
        )
        .await
        .unwrap();
    }

    /// Pin a transcript row to a known instant AFTER the production writer stamped the clock.
    pub(crate) async fn pin_transcript_row(&self, id: i64, at_ms: i64) {
        sqlx::query("UPDATE harness_items SET created_at_ms = ?1 WHERE id = ?2")
            .bind(at_ms)
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// A `harness.item.added` event as the run loop's `emit_item_added` shapes it.
    pub(crate) fn item_added(
        track: &str,
        card: &str,
        method: &str,
        item_type: Option<&str>,
    ) -> Event {
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

    pub(crate) fn track_scope(&self, track_id: &str) -> EventScope {
        EventScope::Track {
            track: TrackId::from(track_id.to_string()),
            area: AreaId::from(self.area_id.clone()),
        }
    }

    pub(crate) fn envelope(scope: EventScope, event: Event) -> BroadcastEnvelope {
        BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::Kernel,
            scope,
            event,
        }
    }
}
