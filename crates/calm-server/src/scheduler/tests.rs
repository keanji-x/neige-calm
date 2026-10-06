use super::*;
use crate::db::sqlite::CheckoutOccupancy;
use crate::model::TaskAccess;

#[test]
fn acceptance_11_sub_track_sweep_arm_precedes_terminal_and_timeout_arms() {
    let source = include_str!("mod.rs");
    let sub_track = source
        .find("TaskStatus::Running if task.spawn == \"sub-wave\"")
        .expect("sub-wave running arm");
    let terminal = source
        .find("TaskStatus::Running if task.kind == TaskKind::Terminal")
        .expect("terminal running arm");
    let timeout = source
        .find("TaskStatus::Running if task_has_running_liveness_deadline(&task)")
        .expect("kind timeout arm");
    assert!(sub_track < terminal && terminal < timeout);
}

#[test]
fn claim_fence_revision_grid_fails_closed_for_missing_null_and_negative() {
    assert!(fence_revision_matches(Some(Some(7)), 7));
    assert!(!fence_revision_matches(None, 7), "missing row");
    assert!(!fence_revision_matches(Some(None), 7), "SQL NULL");
    assert!(
        !fence_revision_matches(Some(Some(-1)), 7),
        "negative revision"
    );
    assert!(
        !fence_revision_matches(Some(Some(8)), 7),
        "changed revision"
    );
}

fn task(key: &str, status: TaskStatus, deps: &[&str], priority: i64) -> Task {
    Task {
        id: format!("w:{key}"),
        track_id: "w".into(),
        key: key.into(),
        kind: TaskKind::Codex,
        goal: "do".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: serde_json::to_string(deps).unwrap(),
        priority,
        gate_json: None,
        status,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        access: crate::model::TaskAccess::ReadWrite,
        start: crate::model::TaskStart::Checkout,
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    }
}

fn keys(tasks: &[Task]) -> Vec<&str> {
    tasks.iter().map(|t| t.key.as_str()).collect()
}

#[test]
fn design_claim_resolution_failure_scope_is_consistent() {
    let design = include_str!("../../../../docs/architecture/985-doc-as-plan.md");
    assert!(design.contains("claim 前定位失败一律不下判决"));
    assert!(!design.contains("越 area、超预算、确定性定位失败一律直接判"));
}

#[test]
fn ready_set_requires_all_deps_done() {
    let tasks = vec![
        task("a", TaskStatus::Done, &[], 0),
        task("b", TaskStatus::Pending, &["a"], 0),
        task("c", TaskStatus::Pending, &["a", "b"], 0),
        task("d", TaskStatus::Pending, &["ghost"], 0),
    ];
    let ready = compute_ready(&tasks, CheckoutOccupancy::Free);
    assert_eq!(keys(&ready), vec!["b"], "only b has all deps done");
}

#[test]
fn canceled_and_failed_deps_never_satisfy() {
    // Deps require `done`; canceled/failed block successors forever.
    let tasks = vec![
        task("a", TaskStatus::Canceled, &[], 0),
        task("b", TaskStatus::Failed, &[], 0),
        task("c", TaskStatus::Pending, &["a"], 0),
        task("d", TaskStatus::Pending, &["b"], 0),
    ];
    assert!(compute_ready(&tasks, CheckoutOccupancy::Free).is_empty());
}

/// #1830 S2 D5, #2139 R2: a codex, claude or terminal task in the track's checkout is ready only
/// while the track is idle (the claim tx then lets one of them win); child-track tasks are not held.
#[test]
fn in_tree_tasks_are_ready_only_while_the_track_is_idle() {
    let mut terminal = task("c-terminal", TaskStatus::Pending, &[], 0);
    terminal.kind = TaskKind::Terminal;
    let mut child = task("e-child", TaskStatus::Pending, &[], 0);
    child.spawn = "sub-wave".into();
    let mut claude = task("b-claude", TaskStatus::Pending, &[], 0);
    claude.kind = TaskKind::Claude;
    let tasks = vec![
        task("a-codex", TaskStatus::Pending, &[], 0),
        claude,
        terminal,
        child,
    ];
    assert_eq!(
        keys(&compute_ready(&tasks, CheckoutOccupancy::Free)),
        vec!["a-codex", "b-claude", "c-terminal", "e-child"],
        "an idle track offers every in-tree task, in scheduler order"
    );
    assert_eq!(
        keys(&compute_ready(&tasks, CheckoutOccupancy::Busy)),
        vec!["e-child"],
        "a busy track admits no in-tree task"
    );
}

/// #2139 R2: a terminal task queues for the checkout by its access, like a codex task: one that
/// changes the checkout waits while a codex task holds it, and a read-only one joins read-only
/// codex tasks.
#[test]
fn a_terminal_task_queues_for_the_checkout_by_its_access() {
    let mut writer = task("t-writer", TaskStatus::Pending, &[], 0);
    writer.kind = TaskKind::Terminal;
    let mut reader_terminal = reader("t-reader");
    reader_terminal.kind = TaskKind::Terminal;
    let waits = |tasks: &[Task], occupancy| {
        crate::db::sqlite::checkout_admission(tasks, occupancy)
            .into_iter()
            .map(|(task, wait)| (task.key.clone(), wait))
            .collect::<Vec<_>>()
    };
    let in_use = Some(crate::db::sqlite::CheckoutWait::InUse);
    for occupancy in [CheckoutOccupancy::Busy, CheckoutOccupancy::Readers] {
        assert_eq!(
            waits(std::slice::from_ref(&writer), occupancy),
            vec![("t-writer".to_string(), in_use)],
            "{occupancy:?}: a terminal task that changes the checkout waits for it"
        );
    }
    assert_eq!(
        waits(
            std::slice::from_ref(&reader_terminal),
            CheckoutOccupancy::Readers
        ),
        vec![("t-reader".to_string(), None)],
        "a read-only terminal task runs beside read-only codex tasks"
    );
    assert_eq!(
        waits(
            std::slice::from_ref(&reader_terminal),
            CheckoutOccupancy::Busy
        ),
        vec![("t-reader".to_string(), in_use)],
        "a read-only terminal task waits while a codex task changes the checkout"
    );
    assert_eq!(
        waits(std::slice::from_ref(&writer), CheckoutOccupancy::Free),
        vec![("t-writer".to_string(), None)]
    );
}

fn reader(key: &str) -> Task {
    let mut task = task(key, TaskStatus::Pending, &[], 0);
    task.access = TaskAccess::ReadOnly;
    task
}

/// #1917: read-only tasks share a free checkout or one only readers use; a task that changes the
/// checkout needs it free. Every admitted task is offered and the claim tx picks (module docs).
#[test]
fn readers_share_the_checkout_and_a_writer_needs_it_free() {
    let tasks = vec![reader("r1"), reader("r2")];
    for occupancy in [CheckoutOccupancy::Free, CheckoutOccupancy::Readers] {
        assert_eq!(
            keys(&compute_ready(&tasks, occupancy)),
            vec!["r1", "r2"],
            "{occupancy:?} admits every reader"
        );
    }
    assert!(compute_ready(&tasks, CheckoutOccupancy::Busy).is_empty());
    let writer = vec![task("w", TaskStatus::Pending, &[], 0)];
    assert_eq!(
        keys(&compute_ready(&writer, CheckoutOccupancy::Free)),
        vec!["w"]
    );
    assert!(
        compute_ready(&writer, CheckoutOccupancy::Readers).is_empty(),
        "a writer waits for the readers"
    );
}

/// #1917 no starvation: once a deps-ready writer waits, every later reader waits behind it;
/// readers ahead of it still run, and a writer whose dependency is not done holds nobody back.
#[test]
fn a_waiting_writer_holds_back_the_readers_after_it() {
    let mut blocked_writer = task("b-blocked", TaskStatus::Pending, &["ghost"], 0);
    blocked_writer.priority = 9;
    let tasks = vec![
        blocked_writer,
        reader("r1"),
        task("w", TaskStatus::Pending, &[], 0),
        reader("r2"),
    ];
    assert_eq!(
        keys(&compute_ready(&tasks, CheckoutOccupancy::Readers)),
        vec!["r1"]
    );
    let admission = crate::db::sqlite::checkout_admission(&tasks, CheckoutOccupancy::Readers);
    let waits: Vec<_> = admission
        .iter()
        .map(|(task, wait)| (task.key.as_str(), *wait))
        .collect();
    assert_eq!(
        waits,
        vec![
            ("r1", None),
            ("w", Some(crate::db::sqlite::CheckoutWait::InUse)),
            ("r2", Some(crate::db::sqlite::CheckoutWait::WriterAhead)),
        ]
    );
    // A free checkout offers all three; the claims pick (the reader's claim then refuses the
    // writer, and the writer's claim the other reader).
    assert_eq!(
        keys(&compute_ready(&tasks, CheckoutOccupancy::Free)),
        vec!["r1", "w", "r2"]
    );
}

#[test]
fn ready_set_preserves_scheduler_order() {
    // Input order is the repo's; compute_ready must not reorder.
    let terminal = |key: &str, priority: i64| {
        let mut task = task(key, TaskStatus::Pending, &[], priority);
        task.kind = TaskKind::Terminal;
        task
    };
    let mut high = terminal("zz-high", 9);
    high.created_at_ms = 5;
    let tasks = vec![high, terminal("aa-low", 0), terminal("bb-low", 0)];
    let ready = compute_ready(&tasks, CheckoutOccupancy::Free);
    assert_eq!(keys(&ready), vec!["zz-high", "aa-low", "bb-low"]);
}

/// The scheduling gate: an open track schedules, a closed one does not.
#[test]
fn only_an_open_track_schedules() {
    let mut track: Track = serde_json::from_value(json!({
        "id": "t", "area_id": "a", "title": "t", "sort": 0.0,
        "pinned_at": null, "closed_at": null, "created_at": 0, "updated_at": 0
    }))
    .unwrap();
    assert!(track.is_open());
    track.closed_at = Some(1);
    assert!(!track.is_open(), "a closed track must not schedule");
}

#[test]
fn reconcile_secs_from_env_fallback_paths() {
    let saved = std::env::var("NEIGE_SCHEDULER_RECONCILE_SECS").ok();
    fn set(v: &str) {
        // SAFETY: single-threaded test; no concurrent env reader.
        unsafe { std::env::set_var("NEIGE_SCHEDULER_RECONCILE_SECS", v) };
    }
    fn remove() {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var("NEIGE_SCHEDULER_RECONCILE_SECS") };
    }

    remove();
    assert_eq!(Scheduler::reconcile_secs_from_env(300), 300);
    set("0");
    assert_eq!(Scheduler::reconcile_secs_from_env(300), 300);
    set("17");
    assert_eq!(Scheduler::reconcile_secs_from_env(300), 17);
    assert_eq!(
        Scheduler::reconcile_secs_from_env_var("NEIGE_SCHEDULER_RECONCILE_SECS", 300),
        17
    );

    match saved {
        Some(v) => set(&v),
        None => remove(),
    }
}

#[test]
fn task_liveness_timeout_env_fallback_paths() {
    let saved_run = std::env::var("NEIGE_TASK_RUN_TIMEOUT_SECS").ok();
    fn set(var: &str, v: &str) {
        // SAFETY: single-threaded test; no concurrent env reader.
        unsafe { std::env::set_var(var, v) };
    }
    fn remove(var: &str) {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(var) };
    }

    remove("NEIGE_TASK_RUN_TIMEOUT_SECS");
    assert_eq!(
        Scheduler::task_run_timeout_from_env(),
        Duration::from_secs(DEFAULT_TASK_RUN_TIMEOUT_SECS)
    );
    set("NEIGE_TASK_RUN_TIMEOUT_SECS", "47");
    assert_eq!(
        Scheduler::task_run_timeout_from_env(),
        Duration::from_secs(47)
    );
    set("NEIGE_TASK_RUN_TIMEOUT_SECS", "-1");
    assert_eq!(
        Scheduler::task_run_timeout_from_env(),
        Duration::from_secs(DEFAULT_TASK_RUN_TIMEOUT_SECS)
    );

    match saved_run {
        Some(v) => set("NEIGE_TASK_RUN_TIMEOUT_SECS", &v),
        None => remove("NEIGE_TASK_RUN_TIMEOUT_SECS"),
    }
}

#[test]
fn worker_payload_is_pure_function_of_the_row() {
    let codex = task("a", TaskStatus::Pending, &[], 0);
    let (kind1, p1) = build_worker_payload(&codex).unwrap();
    let (kind2, p2) = build_worker_payload(&codex).unwrap();
    assert_eq!(kind1, "codex-worker");
    assert_eq!(kind1, kind2);
    assert_eq!(p1, p2, "same row → byte-identical payload");
    assert_eq!(
        stable_payload_hash(&p1).unwrap(),
        stable_payload_hash(&p2).unwrap(),
        "same row → same idempotency payload hash (post-crash resubmit matches)"
    );
    assert_eq!(p1["idempotency_key"], json!("w:a"));
    assert_eq!(
        p1["actor"],
        serde_json::to_value(ActorId::KernelDispatcher).unwrap()
    );
    assert!(
        !p1.as_object().unwrap().contains_key("cwd"),
        "codex cwd stays absent; prepare_tx supplies the lease cwd"
    );

    let mut claude = task("cl", TaskStatus::Pending, &[], 0);
    claude.kind = TaskKind::Claude;
    claude.cwd = Some("/repo/from-plan".into());
    let (kind, p) = build_worker_payload(&claude).unwrap();
    assert_eq!(kind, "claude-worker");
    assert_eq!(p["idempotency_key"], json!("w:cl"));
    assert_eq!(
        p["actor"],
        serde_json::to_value(ActorId::KernelDispatcher).unwrap()
    );
    assert!(
        !p.as_object().unwrap().contains_key("cwd"),
        "claude cwd stays absent; prepare_tx supplies the lease cwd"
    );

    let mut terminal = task("t", TaskStatus::Pending, &[], 0);
    terminal.kind = TaskKind::Terminal;
    terminal.goal = "make test".into();
    terminal.cwd = Some("/repo".into());
    let (kind, p) = build_worker_payload(&terminal).unwrap();
    assert_eq!(kind, "terminal-worker");
    assert_eq!(p["cmd"], json!("make test"));
    assert_eq!(p["cwd"], json!("/repo"));
}

#[test]
fn codex_payload_ignores_task_cwd_for_hash_stability() {
    let mut codex = task("a", TaskStatus::Pending, &[], 0);
    codex.cwd = Some("/repo".into());
    let (kind, p) = build_worker_payload(&codex).unwrap();
    assert_eq!(kind, "codex-worker");
    assert!(
        !p.as_object().unwrap().contains_key("cwd"),
        "task.cwd must not affect codex worker payload identity"
    );

    let legacy_without_cwd = json!({
        "actor": serde_json::to_value(ActorId::KernelDispatcher).unwrap(),
        "track_id": "w",
        "idempotency_key": "w:a",
        "goal": "do",
        "context": null,
    });
    assert_eq!(
        stable_payload_hash(&p).unwrap(),
        stable_payload_hash(&legacy_without_cwd).unwrap(),
        "non-null task.cwd must hash like the pre-upgrade no-cwd payload"
    );

    codex.cwd = None;
    let (_, p1) = build_worker_payload(&codex).unwrap();
    assert_eq!(p, p1);
    assert_eq!(
        stable_payload_hash(&p).unwrap(),
        stable_payload_hash(&p1).unwrap()
    );
}

#[test]
fn claude_payload_ignores_task_cwd_for_hash_stability() {
    let mut claude = task("a", TaskStatus::Pending, &[], 0);
    claude.kind = TaskKind::Claude;
    claude.cwd = Some("/repo".into());
    let (kind, p) = build_worker_payload(&claude).unwrap();
    assert_eq!(kind, "claude-worker");
    assert!(
        !p.as_object().unwrap().contains_key("cwd"),
        "task.cwd must not affect claude worker payload identity"
    );

    let legacy_without_cwd = json!({
        "actor": serde_json::to_value(ActorId::KernelDispatcher).unwrap(),
        "track_id": "w",
        "idempotency_key": "w:a",
        "goal": "do",
        "context": null,
    });
    assert_eq!(
        stable_payload_hash(&p).unwrap(),
        stable_payload_hash(&legacy_without_cwd).unwrap(),
        "non-null task.cwd must hash like the no-cwd payload"
    );

    claude.cwd = None;
    let (_, p1) = build_worker_payload(&claude).unwrap();
    assert_eq!(p, p1);
    assert_eq!(
        stable_payload_hash(&p).unwrap(),
        stable_payload_hash(&p1).unwrap()
    );
}

#[test]
fn task_kind_str_includes_claude() {
    assert_eq!(task_kind_str(TaskKind::Codex), "codex");
    assert_eq!(task_kind_str(TaskKind::Claude), "claude");
    assert_eq!(task_kind_str(TaskKind::Terminal), "terminal");
}

#[test]
fn terminal_payload_without_cwd_keeps_row_none() {
    // A terminal row with `cwd = NULL` must produce `cwd: null`, NOT a materialized
    // `default_cwd()`: anything env-derived would change `stable_payload_hash` across a restart.
    let mut terminal = task("t", TaskStatus::Dispatched, &[], 0);
    terminal.kind = TaskKind::Terminal;
    terminal.goal = "make test".into();
    terminal.cwd = None;
    let (kind, p1) = build_worker_payload(&terminal).unwrap();
    assert_eq!(kind, "terminal-worker");
    assert_eq!(p1["cwd"], Value::Null, "row None stays None");
    // Restart simulation: the same frozen row must rebuild a byte-identical payload.
    let (_, p2) = build_worker_payload(&terminal).unwrap();
    assert_eq!(p1, p2);
    assert_eq!(
        stable_payload_hash(&p1).unwrap(),
        stable_payload_hash(&p2).unwrap()
    );
}

#[tokio::test]
async fn inflight_guard_is_single_flight_and_releases_on_drop() {
    let map: Arc<DashMap<String, ()>> = Arc::new(DashMap::new());
    let g1 = InflightGuard::acquire(&map, "w:a").expect("first acquire");
    assert!(
        InflightGuard::acquire(&map, "w:a").is_none(),
        "second concurrent acquire must lose"
    );
    assert!(
        InflightGuard::acquire(&map, "w:b").is_some(),
        "other keys independent"
    );
    drop(g1);
    assert!(
        InflightGuard::acquire(&map, "w:a").is_some(),
        "slot frees on drop"
    );
}

#[tokio::test]
async fn sweep_running_claude_past_liveness_deadline_fails_and_releases_lease_row() {
    let concrete = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open repo"),
    );
    let repo: Arc<dyn Repo> = concrete.clone();
    let route_repo: Arc<dyn crate::db::RouteRepo> = concrete.clone();
    let area = repo
        .area_create(crate::model::NewArea {
            name: "claude-timeout".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo
        .track_create(crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "claude-timeout".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create track");

    let pool = concrete.pool().clone();
    let now = now_ms();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool)
        .await
        .expect("begin seed tx");
    let (card, _term) = calm_truth::db::sqlite::card_with_claude_worker_create_tx(
        &mut tx,
        "card-claude-timeout".into(),
        "runtime-claude-timeout",
        None,
        track.id.clone(),
        None,
        None,
        "claude".into(),
        "/tmp".into(),
        json!({}),
        Some("do".into()),
        None,
        None,
        "/tmp/neige-claude-timeout-settings.json".into(),
        "claude-session-timeout".into(),
        concrete.card_role_cache(),
        crate::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("create claude worker card");
    let mut running = task("claude-timeout", TaskStatus::Running, &[], 0);
    running.id = format!("{}:claude-timeout", track.id.as_str());
    running.track_id = track.id.as_str().to_string();
    running.kind = TaskKind::Claude;
    running.worker_card_id = Some(card.id.to_string());
    running.running_deadline_ms = Some(now - 1);
    running.created_at_ms = now;
    running.updated_at_ms = now;
    let task_id = running.id.clone();
    crate::test_support::insert_task_tx(&mut tx, &running)
        .await
        .expect("insert running claude task");
    tx.commit().await.expect("commit seed tx");

    sqlx::query(
        r#"UPDATE worker_sessions
               SET state = 'running',
                   updated_at_ms = ?1
               WHERE id = 'runtime-claude-timeout'"#,
    )
    .bind(now)
    .execute(&pool)
    .await
    .expect("mark claude session running");
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, track_id, path, state, lease_owner, lease_until_ms,
                   boot_id, created_at_ms, updated_at_ms
               )
               VALUES ('lease-claude-timeout', ?1, ?2, '/tmp/neige-claude-timeout-lease',
                       'held', 'test-owner', ?3, NULL, ?4, ?4)"#,
    )
    .bind(card.id.as_ref())
    .bind(track.id.as_str())
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert held lease");

    let events = EventBus::new();
    let write = WriteContext::new(
        concrete.card_role_cache().clone(),
        concrete.track_area_cache().clone(),
    );
    let operation_repo = Arc::new(crate::operation::SqlxOperationRepo::new(pool.clone()));
    let completion = crate::operation::OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        Vec::new(),
        events.clone(),
        completion.clone(),
        crate::operation::SpawnCtx::new(
            route_repo.clone(),
            operation_repo,
            Arc::new(crate::state::DaemonClient::new_stub()),
            crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(route_repo),
            events.clone(),
            completion,
        ),
    ));
    let scheduler = Scheduler::new(
        repo.clone(),
        events,
        write,
        Arc::downgrade(&runtime),
        crate::per_card_lock::new_per_card_locks(),
        Arc::new(Semaphore::new(1)),
        std::env::temp_dir().join("neige-scheduler-test-gate-logs"),
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    scheduler.mark_boot_sweep_complete();
    scheduler.open_context_sweep_gate().await;

    scheduler.sweep_all().await;

    let failed = repo
        .task_get(&task_id)
        .await
        .expect("read task")
        .expect("task row");
    assert_eq!(failed.status, TaskStatus::Failed);
    assert_eq!(failed.status_detail.as_deref(), Some("worker-timeout"));
    let lease_state: String = sqlx::query_scalar(
        "SELECT state FROM workspace_leases WHERE lease_id = 'lease-claude-timeout'",
    )
    .fetch_one(&pool)
    .await
    .expect("lease state");
    assert_eq!(lease_state, "released");
    let session_state: String =
        sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = 'runtime-claude-timeout'")
            .fetch_one(&pool)
            .await
            .expect("session state");
    assert_eq!(session_state, "failed");
    let cleanup_markers: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
               FROM worker_sessions
               WHERE id = 'runtime-claude-timeout'
                 AND json_extract(handle_state_json, '$.timeout_cleanup.requested_at_ms')
                     IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .expect("cleanup marker count");
    assert_eq!(cleanup_markers, 0, "cleanup marker must be cleared");
}

#[tokio::test]
async fn running_timeout_race_lost_does_not_teardown_or_release_lease() {
    let concrete = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open repo"),
    );
    let repo: Arc<dyn Repo> = concrete.clone();
    let area = repo
        .area_create(crate::model::NewArea {
            name: "timeout-race".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo
        .track_create(crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "timeout-race".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    let mut stored = task("race", TaskStatus::Done, &[], 0);
    stored.id = format!("{}:race", track.id.as_str());
    stored.track_id = track.id.as_str().to_string();
    stored.worker_card_id = Some("card-race".into());
    let mut snapshot = stored.clone();
    snapshot.status = TaskStatus::Running;
    snapshot.running_deadline_ms = Some(now_ms() - 1);

    let pool = concrete.pool().clone();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool)
        .await
        .expect("begin task tx");
    crate::test_support::insert_task_tx(&mut tx, &stored)
        .await
        .expect("insert done task");
    tx.commit().await.expect("commit task tx");

    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, track_id, path, state, lease_owner, lease_until_ms,
                   boot_id, created_at_ms, updated_at_ms
               )
               VALUES ('lease-race', 'card-race', ?1, '/tmp/neige-timeout-race',
                       'held', 'test-owner', ?2, NULL, ?3, ?3)"#,
    )
    .bind(track.id.as_str())
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert held lease");
    sqlx::query(
        r#"INSERT INTO worker_sessions (
                   id, track_id, provider, mode, contract, state, card_id,
                   created_at_ms, updated_at_ms
               )
               VALUES ('runtime-race', ?1, 'codex', 'resumable', 'executor',
                       'running', 'card-race', ?2, ?2)"#,
    )
    .bind(track.id.as_str())
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert worker session");

    let events = EventBus::new();
    let write = WriteContext::new(
        concrete.card_role_cache().clone(),
        concrete.track_area_cache().clone(),
    );
    let scheduler = Scheduler::new(
        repo,
        events,
        write,
        Weak::<OperationRuntime>::new(),
        crate::per_card_lock::new_per_card_locks(),
        Arc::new(Semaphore::new(1)),
        std::env::temp_dir().join("neige-scheduler-test-gate-logs"),
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(concrete.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );

    scheduler.fail_running_liveness_timeout(snapshot).await;

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = 'lease-race'")
            .fetch_one(&pool)
            .await
            .expect("lease state");
    assert_eq!(state, "held", "0-row CAS must not release lease");
    let failed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'task.failed'")
            .fetch_one(&pool)
            .await
            .expect("failed event count");
    assert_eq!(failed_events, 0, "0-row CAS must not emit task.failed");
    let cleanup_markers: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
               FROM worker_sessions
               WHERE card_id = 'card-race'
                 AND json_extract(handle_state_json, '$.timeout_cleanup.requested_at_ms')
                     IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .expect("cleanup marker count");
    assert_eq!(cleanup_markers, 0, "0-row CAS must not mark cleanup");
}
