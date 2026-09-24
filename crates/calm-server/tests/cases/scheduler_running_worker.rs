//! #1785 S1: `calm.plan.cancel` on a running Track worker, and the sweep's idle arm that fails a
//! codex task whose worker turn ended without a report.

use super::*;

use calm_provider::provider::{CodexDaemonProbe, CodexLivenessFacts, ThreadStatusLite};
use calm_server::dispatcher::task_event_pushes_planner_for_test;
use calm_server::event::BroadcastEnvelope;
use calm_server::mcp_server::tools::plan::TOOL_PLAN_CANCEL;

/// r4 of #1772: the rollout's `turn_aborted` `completed_at` (Unix SECONDS) = 2026-09-23
/// 18:41:24 +08:00 (10:41:24Z).
const R4_COMPLETED_AT_S: i64 = 1_790_160_084;
const R4_COMPLETED_AT_MS: i64 = R4_COMPLETED_AT_S * 1000;

/// What the scripted daemon answers for the candidate thread.
#[derive(Clone)]
enum Answer {
    Facts(CodexLivenessFacts),
    Unreachable,
    Hang,
}

struct ScriptedProbe {
    answer: Answer,
    active_turn: Option<String>,
    reads: std::sync::Mutex<Vec<String>>,
}

impl ScriptedProbe {
    fn new(answer: Answer, active_turn: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            active_turn: active_turn.map(str::to_string),
            reads: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn reads(&self) -> Vec<String> {
        self.reads.lock().unwrap().clone()
    }
}

#[async_trait]
impl CodexDaemonProbe for ScriptedProbe {
    fn is_running(&self) -> bool {
        true
    }

    fn active_turn_id_for_thread(&self, _thread_id: &str) -> Option<String> {
        self.active_turn.clone()
    }

    fn remote_uri(&self) -> String {
        String::new()
    }

    fn daemon_connected_at_ms(&self) -> i64 {
        0
    }

    async fn read_liveness_facts(&self, thread_id: &str) -> Option<CodexLivenessFacts> {
        self.reads.lock().unwrap().push(thread_id.to_string());
        match &self.answer {
            Answer::Facts(facts) => Some(*facts),
            Answer::Unreachable => None,
            Answer::Hang => std::future::pending().await,
        }
    }
}

fn thread_facts(
    status: ThreadStatusLite,
    last_turn_completed_at: Option<Option<i64>>,
) -> CodexLivenessFacts {
    CodexLivenessFacts {
        loaded: true,
        status,
        last_turn_completed_at,
    }
}

fn r4_facts() -> CodexLivenessFacts {
    thread_facts(ThreadStatusLite::Idle, Some(Some(R4_COMPLETED_AT_S)))
}

/// The production grace and probe bound, with `now` pinned.
fn idle_at(probe: Arc<ScriptedProbe>, now_ms: i64) -> WorkerIdleWake {
    WorkerIdleWake::new(probe, WORKER_IDLE_TURN_GRACE, WORKER_IDLE_PROBE_TIMEOUT)
        .with_clock_for_test(Arc::new(move || now_ms))
}

struct IdleWorker {
    task_key: String,
    card_id: String,
    session_row_id: String,
    thread_id: String,
    lease_id: String,
    _lease_dir: tempfile::TempDir,
}

/// A running codex task inside its deadline whose worker session persisted `idle` with no
/// completed-turn stamp, as r4's did (`last_turn_completed_ms` NULL).
async fn seed_idle_codex_worker(boot: &Boot, label: &str, gated: bool) -> IdleWorker {
    let (card_id, session_row_id, _terminal_id) =
        seed_codex_worker_card_with_terminal(boot, label).await;
    let (lease_id, lease_dir) = seed_held_workspace_lease(boot, &card_id, label).await;
    let thread_id = format!("thread-{label}");
    sqlx::query(
        "UPDATE worker_sessions SET thread_id = ?1, last_thread_status = 'idle', \
         last_activity_ms = ?2, last_turn_completed_ms = NULL WHERE id = ?3",
    )
    .bind(&thread_id)
    .bind(R4_COMPLETED_AT_MS)
    .bind(&session_row_id)
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .expect("persist idle thread status");
    let mut task = plan_task(&boot.track_id, label, TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() + 7_200_000);
    if gated {
        task.gate_json = Some(json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string());
    }
    seed_task(boot, task).await;
    IdleWorker {
        task_key: label.to_string(),
        card_id,
        session_row_id,
        thread_id,
        lease_id,
        _lease_dir: lease_dir,
    }
}

/// One reconcile sweep, then wait for the idle rechecks it spawned.
async fn sweep_and_settle_idle_checks(scheduler: &Arc<Scheduler>) {
    scheduler.sweep_all().await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while scheduler.worker_idle_checks_in_flight() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle rechecks settle");
}

async fn session_state(boot: &Boot, session_row_id: &str) -> WorkerSessionState {
    boot.repo
        .session_projection_by_id(session_row_id)
        .await
        .expect("runtime lookup")
        .expect("runtime row")
        .status
}

fn task_failed_envelopes(
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
) -> Vec<BroadcastEnvelope> {
    let mut seen = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        if matches!(envelope.event, Event::TaskFailed { .. }) {
            seen.push(envelope);
        }
    }
    seen
}

/// The worker was left exactly as seeded: nothing failed, reaped or released.
async fn assert_untouched(boot: &Boot, worker: &IdleWorker) {
    let row = task_row(boot, &worker.task_key).await;
    assert_eq!(row.status, TaskStatus::Running);
    assert_eq!(row.status_detail, None);
    assert_eq!(
        session_state(boot, &worker.session_row_id).await,
        WorkerSessionState::Running
    );
    assert_eq!(workspace_lease_state(boot, &worker.lease_id).await, "held");
    assert!(!timeout_cleanup_marker_exists(boot, &worker.card_id).await);
    assert!(event_rows(boot, "task.failed").await.is_empty());
}

/// The idle arm failed the task as `worker-turn-ended`, reaped the worker and pushed the wake.
async fn assert_turn_ended_and_woken(
    boot: &Boot,
    worker: &IdleWorker,
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
) {
    let row = task_row(boot, &worker.task_key).await;
    assert_eq!(row.status, TaskStatus::Failed);
    assert_eq!(row.status_detail.as_deref(), Some("worker-turn-ended"));
    assert_eq!(
        session_state(boot, &worker.session_row_id).await,
        WorkerSessionState::Failed
    );
    assert_eq!(
        workspace_lease_state(boot, &worker.lease_id).await,
        "released"
    );
    assert!(!timeout_cleanup_marker_exists(boot, &worker.card_id).await);
    let failed = task_failed_envelopes(rx);
    assert_eq!(failed.len(), 1, "exactly one task.failed");
    assert_eq!(failed[0].actor, ActorId::KernelDispatcher);
    assert!(
        task_event_pushes_planner_for_test(
            boot.repo.as_ref(),
            &boot.write,
            &failed[0].event,
            &failed[0].actor
        )
        .await,
        "the worker-turn-ended failure must wake the Planner"
    );
}

async fn run_r4_past_grace(gated: bool, label: &str) {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_idle_codex_worker(&boot, label, gated).await;
    let probe = ScriptedProbe::new(Answer::Facts(r4_facts()), None);
    // 18:46:25 +08:00: 301 s after the turn ended.
    let (_runtime, scheduler) =
        build_scheduler_with_idle(&boot, idle_at(probe.clone(), R4_COMPLETED_AT_MS + 301_000));
    let mut rx = boot.events.subscribe();

    sweep_and_settle_idle_checks(&scheduler).await;

    assert_eq!(probe.reads(), vec![worker.thread_id.clone()]);
    assert_turn_ended_and_woken(&boot, &worker, &mut rx).await;
}

#[tokio::test]
async fn r4_idle_worker_past_grace_fails_as_turn_ended_and_wakes_planner() {
    run_r4_past_grace(false, "r4-past-grace").await;
}

#[tokio::test]
async fn r4_idle_gated_worker_past_grace_fails_as_turn_ended_and_wakes_planner() {
    run_r4_past_grace(true, "r4-gated-past-grace").await;
}

#[tokio::test]
async fn r4_idle_worker_within_grace_is_untouched() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_idle_codex_worker(&boot, "r4-within-grace", false).await;
    let probe = ScriptedProbe::new(Answer::Facts(r4_facts()), None);
    // 18:46:23 +08:00: 299 s after the turn ended.
    let (_runtime, scheduler) =
        build_scheduler_with_idle(&boot, idle_at(probe.clone(), R4_COMPLETED_AT_MS + 299_000));

    sweep_and_settle_idle_checks(&scheduler).await;

    assert_eq!(probe.reads(), vec![worker.thread_id.clone()]);
    assert_untouched(&boot, &worker).await;
}

#[tokio::test]
async fn idle_turn_that_ended_before_the_running_stamp_is_detected() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_idle_codex_worker(&boot, "ended-before-running", false).await;
    let now = now_ms();
    // Turn ended 400 s ago; the running stamp (deadline - 2 h) landed 100 s ago.
    sqlx::query("UPDATE tasks SET running_deadline_ms = ?1 WHERE key = ?2")
        .bind(now - 100_000 + 7_200_000)
        .bind(&worker.task_key)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let completed_s = (now - 400_000) / 1000;
    let probe = ScriptedProbe::new(
        Answer::Facts(thread_facts(
            ThreadStatusLite::Idle,
            Some(Some(completed_s)),
        )),
        None,
    );
    let (_runtime, scheduler) = build_scheduler_with_idle(
        &boot,
        WorkerIdleWake::new(
            probe.clone(),
            WORKER_IDLE_TURN_GRACE,
            WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    let mut rx = boot.events.subscribe();

    sweep_and_settle_idle_checks(&scheduler).await;

    assert_turn_ended_and_woken(&boot, &worker, &mut rx).await;
}

#[tokio::test]
async fn idle_recheck_ignores_a_failed_loaded_list() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_idle_codex_worker(&boot, "not-loaded-listed", false).await;
    let mut facts = r4_facts();
    facts.loaded = false;
    let probe = ScriptedProbe::new(Answer::Facts(facts), None);
    let (_runtime, scheduler) =
        build_scheduler_with_idle(&boot, idle_at(probe, R4_COMPLETED_AT_MS + 301_000));
    let mut rx = boot.events.subscribe();

    sweep_and_settle_idle_checks(&scheduler).await;

    assert_turn_ended_and_woken(&boot, &worker, &mut rx).await;
}

/// Persisted `idle`, clock past the grace; only the live recheck says no.
async fn assert_live_recheck_vetoes(label: &str, answer: Answer, active_turn: Option<&str>) {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_idle_codex_worker(&boot, label, false).await;
    let probe = ScriptedProbe::new(answer, active_turn);
    let mut idle = idle_at(probe.clone(), R4_COMPLETED_AT_MS + 301_000);
    if matches!(probe.answer, Answer::Hang) {
        idle = WorkerIdleWake::new(
            probe.clone(),
            WORKER_IDLE_TURN_GRACE,
            Duration::from_millis(200),
        )
        .with_clock_for_test(Arc::new(|| R4_COMPLETED_AT_MS + 301_000));
    }
    let (_runtime, scheduler) = build_scheduler_with_idle(&boot, idle);

    sweep_and_settle_idle_checks(&scheduler).await;

    assert_eq!(probe.reads(), vec![worker.thread_id.clone()]);
    assert_untouched(&boot, &worker).await;
}

#[tokio::test]
async fn idle_candidate_whose_live_thread_is_active_is_untouched() {
    let active = ThreadStatusLite::Active {
        waiting_on_user_input: false,
        waiting_on_approval: false,
    };
    let facts = thread_facts(active, Some(Some(R4_COMPLETED_AT_S)));
    assert_live_recheck_vetoes("live-active", Answer::Facts(facts), None).await;
}

#[tokio::test]
async fn idle_candidate_whose_live_thread_has_no_turn_is_untouched() {
    let facts = thread_facts(ThreadStatusLite::Idle, None);
    assert_live_recheck_vetoes("live-no-turn", Answer::Facts(facts), None).await;
}

#[tokio::test]
async fn idle_candidate_whose_live_turn_never_finished_is_untouched() {
    let facts = thread_facts(ThreadStatusLite::Idle, Some(None));
    assert_live_recheck_vetoes("live-unfinished", Answer::Facts(facts), None).await;
}

#[tokio::test]
async fn idle_candidate_with_an_active_turn_is_untouched() {
    assert_live_recheck_vetoes(
        "live-active-turn",
        Answer::Facts(r4_facts()),
        Some("turn-live"),
    )
    .await;
}

#[tokio::test]
async fn idle_candidate_whose_thread_read_fails_is_untouched() {
    assert_live_recheck_vetoes("live-unreachable", Answer::Unreachable, None).await;
}

#[tokio::test]
async fn idle_candidate_whose_thread_read_times_out_is_untouched() {
    assert_live_recheck_vetoes("live-hang", Answer::Hang, None).await;
}

#[tokio::test]
async fn idle_arm_leaves_claude_and_isolated_workers_alone() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let claude = seed_idle_codex_worker(&boot, "claude-idle", false).await;
    sqlx::query("UPDATE tasks SET kind = 'claude' WHERE key = 'claude-idle'")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let isolated = seed_idle_codex_worker(&boot, "isolated-idle", false).await;
    seed_worker_op_target(
        &boot,
        "codex-isolated-worker",
        &format!("{}:isolated-idle", boot.track_id.as_str()),
        &isolated.card_id,
    )
    .await;
    let probe = ScriptedProbe::new(Answer::Facts(r4_facts()), None);
    let (_runtime, scheduler) =
        build_scheduler_with_idle(&boot, idle_at(probe.clone(), R4_COMPLETED_AT_MS + 301_000));

    sweep_and_settle_idle_checks(&scheduler).await;

    assert!(probe.reads().is_empty(), "no recheck: {:?}", probe.reads());
    for worker in [&claude, &isolated] {
        assert_eq!(
            task_row(&boot, &worker.task_key).await.status,
            TaskStatus::Running
        );
        assert_eq!(workspace_lease_state(&boot, &worker.lease_id).await, "held");
    }
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}

/// A running codex task whose worker card, shared turn and held lease the reap must clean up.
async fn seed_running_codex_worker(boot: &Boot, label: &str) -> IdleWorker {
    let worker = seed_idle_codex_worker(boot, label, false).await;
    sqlx::query("UPDATE worker_sessions SET last_thread_status = 'active' WHERE id = ?1")
        .bind(&worker.session_row_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    seed_active_codex_turn(boot, &worker.session_row_id, &worker.thread_id, "turn-live").await;
    worker
}

#[tokio::test]
async fn cancel_running_task_cancels_and_reaps_worker() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let worker = seed_running_codex_worker(&boot, "cancel-me").await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    assert!(
        boot.ctx
            .scheduler_poke
            .set(Arc::new(scheduler.clone()))
            .is_ok()
    );

    call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        planner_identity(&boot),
        json!({ "key": "cancel-me", "message": "wrong direction" }),
    )
    .await
    .expect("running codex task is cancelable");

    let row = task_row(&boot, "cancel-me").await;
    assert_eq!(row.status, TaskStatus::Canceled);
    assert_eq!(row.status_detail.as_deref(), Some("planner-canceled"));
    assert!(row.finished_at_ms.is_some());
    // The post-commit poke reaps without a reconcile sweep.
    tokio::time::timeout(Duration::from_secs(20), async {
        while workspace_lease_state(&boot, &worker.lease_id).await != "released" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("poked cleanup releases the lease");
    assert_eq!(
        session_state(&boot, &worker.session_row_id).await,
        WorkerSessionState::Failed
    );
    assert!(
        boot.shared_codex_appserver
            .interrupted_turns_for_test()
            .contains(&(worker.thread_id.clone(), "turn-live".to_string())),
        "the reap interrupts the worker's live turn"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while timeout_cleanup_marker_exists(&boot, &worker.card_id).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cleanup marker clears");
    assert_eq!(event_rows(&boot, "plan.updated").await.len(), 1);
    assert!(event_rows(&boot, "task.failed").await.is_empty());
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        track.lifecycle,
        TrackLifecycle::Working,
        "cancel moves no lifecycle by itself"
    );
}

#[tokio::test]
async fn cancel_running_claude_task_cancels_and_reaps_worker() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let card_id = format!("card-claude-cancel-{}", new_id());
    let session_row_id = format!("runtime-claude-cancel-{}", new_id());
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = pool.begin().await.unwrap();
    calm_server::db::sqlite::card_with_claude_worker_create_tx(
        &mut tx,
        card_id.clone(),
        &session_row_id,
        None,
        boot.track_id.clone(),
        None,
        None,
        "claude".into(),
        "/tmp".into(),
        json!({}),
        Some("do".into()),
        None,
        None,
        "/tmp/neige-claude-cancel-settings.json".into(),
        "claude-session-cancel".into(),
        &boot.card_role_cache,
        RequestTheme::default_dark(),
    )
    .await
    .expect("create claude worker card");
    tx.commit().await.unwrap();
    boot.repo
        .session_projection_set_status_for_card(&card_id, WorkerSessionState::Running)
        .await
        .unwrap();
    let (lease_id, _lease_dir) = seed_held_workspace_lease(&boot, &card_id, "claude").await;
    let mut task = plan_task(&boot.track_id, "claude-cancel", TaskKind::Claude, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(card_id.clone());
    task.running_deadline_ms = Some(now_ms() + 7_200_000);
    seed_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    assert!(
        boot.ctx
            .scheduler_poke
            .set(Arc::new(scheduler.clone()))
            .is_ok()
    );

    call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        planner_identity(&boot),
        json!({ "key": "claude-cancel", "message": "stop" }),
    )
    .await
    .expect("running claude task is cancelable");

    let row = task_row(&boot, "claude-cancel").await;
    assert_eq!(row.status, TaskStatus::Canceled);
    assert_eq!(row.status_detail.as_deref(), Some("planner-canceled"));
    tokio::time::timeout(Duration::from_secs(20), async {
        while workspace_lease_state(&boot, &lease_id).await != "released" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("poked cleanup releases the claude lease");
    assert_eq!(
        session_state(&boot, &session_row_id).await,
        WorkerSessionState::Failed
    );
}

/// A running codex row owned by the boot worker card, so that card's identity can report on it.
async fn seed_running_boot_worker_task(boot: &Boot, key: &str, gated: bool) -> String {
    let mut task = plan_task(&boot.track_id, key, TaskKind::Codex, &[]);
    task.status = TaskStatus::Running;
    task.worker_card_id = Some(boot.worker_card_id.to_string());
    task.running_deadline_ms = Some(now_ms() + 7_200_000);
    if gated {
        task.gate_json = Some(json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string());
    }
    let task_id = task.id.clone();
    seed_task(boot, task).await;
    task_id
}

#[tokio::test]
async fn cancel_first_then_late_worker_report_is_rejected_without_delivery() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let task_id = seed_running_boot_worker_task(&boot, "cancel-first", false).await;

    call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        planner_identity(&boot),
        json!({ "key": "cancel-first", "message": "stop" }),
    )
    .await
    .expect("cancel wins");
    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": {} }),
    )
    .await
    .expect_err("a report after the cancel is rejected");

    let row = task_row(&boot, "cancel-first").await;
    assert_eq!(row.status, TaskStatus::Canceled);
    assert!(event_rows(&boot, "task.completed").await.is_empty());
    let deliveries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM task_git_deliveries WHERE producer_attempt_id = ?1",
    )
    .bind(&task_id)
    .fetch_one(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(deliveries, 0, "a rejected report delivers nothing");
}

#[tokio::test]
async fn report_first_then_cancel_is_refused_with_the_current_status() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Working).await;
    let task_id = seed_running_boot_worker_task(&boot, "report-first", true).await;

    call_tool(
        &boot,
        TOOL_TASK_COMPLETE,
        worker_identity(&boot),
        json!({ "idempotency_key": task_id, "result": {} }),
    )
    .await
    .expect("report wins");
    let err = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        planner_identity(&boot),
        json!({ "key": "report-first", "message": "too late" }),
    )
    .await
    .expect_err("a verifying task is not cancelable");

    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message.contains("task report-first is verifying")
            && err.message.contains("task.gate_result")
            && !err.message.contains("#644"),
        "{err:?}"
    );
    assert_eq!(
        task_row(&boot, "report-first").await.status,
        TaskStatus::Verifying
    );
    assert!(event_rows(&boot, "plan.updated").await.is_empty());
}
