use super::*;

fn task(key: &str, status: TaskStatus, deps: &[&str], priority: i64) -> Task {
    Task {
        id: format!("w:{key}"),
        wave_id: "w".into(),
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
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    }
}

fn keys(tasks: &[Task]) -> Vec<&str> {
    tasks.iter().map(|t| t.key.as_str()).collect()
}

// ---------------------------------------------------- ready set (§5.2)

#[test]
fn ready_set_requires_all_deps_done() {
    let tasks = vec![
        task("a", TaskStatus::Done, &[], 0),
        task("b", TaskStatus::Pending, &["a"], 0),
        task("c", TaskStatus::Pending, &["a", "b"], 0),
        task("d", TaskStatus::Pending, &["ghost"], 0),
    ];
    let ready = compute_ready(&tasks, 10);
    assert_eq!(keys(&ready), vec!["b"], "only b has all deps done");
}

#[test]
fn canceled_and_failed_deps_never_satisfy() {
    // §3.1 — deps require `done`; canceled/failed block successors
    // forever (plan-revision authority belongs to the spec).
    let tasks = vec![
        task("a", TaskStatus::Canceled, &[], 0),
        task("b", TaskStatus::Failed, &[], 0),
        task("c", TaskStatus::Pending, &["a"], 0),
        task("d", TaskStatus::Pending, &["b"], 0),
    ];
    assert!(compute_ready(&tasks, 10).is_empty());
}

#[test]
fn budget_counts_dispatched_running_and_verifying() {
    // `verifying` occupies budget deliberately (§5.2) — the SQL/
    // predicate is future-proofed even though no task reaches
    // verifying before PR-C.
    let tasks = vec![
        task("a", TaskStatus::Dispatched, &[], 0),
        task("b", TaskStatus::Running, &[], 0),
        task("c", TaskStatus::Verifying, &[], 0),
        task("d", TaskStatus::Pending, &[], 0),
        task("e", TaskStatus::Pending, &[], 0),
    ];
    assert!(
        compute_ready(&tasks, 3).is_empty(),
        "3 in flight fill budget 3"
    );
    let ready = compute_ready(&tasks, 4);
    assert_eq!(keys(&ready), vec!["d"], "one free slot under budget 4");
    let ready = compute_ready(&tasks, 5);
    assert_eq!(keys(&ready), vec!["d", "e"]);
}

#[test]
fn ready_set_preserves_scheduler_order_and_caps_at_budget() {
    // Input order is the repo's `(priority DESC, created_at ASC,
    // key ASC)`; compute_ready must not reorder (policy-free).
    let mut high = task("zz-high", TaskStatus::Pending, &[], 9);
    high.created_at_ms = 5;
    let tasks = vec![
        high,
        task("aa-low", TaskStatus::Pending, &[], 0),
        task("bb-low", TaskStatus::Pending, &[], 0),
    ];
    let ready = compute_ready(&tasks, 2);
    assert_eq!(keys(&ready), vec!["zz-high", "aa-low"]);
}

#[test]
fn zero_or_negative_capacity_dispatches_nothing() {
    let tasks = vec![
        task("a", TaskStatus::Running, &[], 0),
        task("b", TaskStatus::Pending, &[], 0),
    ];
    assert!(compute_ready(&tasks, 0).is_empty());
    assert!(
        compute_ready(&tasks, 1).is_empty(),
        "running fills budget 1"
    );
}

// ---------------------------------------------- lifecycle gating (§5.2)

#[test]
fn lifecycle_gating_matches_design_table() {
    for allowed in [
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Reviewing,
    ] {
        assert!(lifecycle_allows_scheduling(allowed), "{allowed:?}");
    }
    for held in [
        WaveLifecycle::Draft,
        WaveLifecycle::Blocked,
        WaveLifecycle::Done,
        WaveLifecycle::Canceled,
        WaveLifecycle::Failed,
    ] {
        assert!(!lifecycle_allows_scheduling(held), "{held:?}");
    }
}

// ------------------------------------------------------- env knobs

#[test]
fn budget_from_env_fallback_paths() {
    let saved = std::env::var("NEIGE_WAVE_TASK_BUDGET").ok();
    fn set(v: &str) {
        // SAFETY: single-threaded test; no concurrent env reader.
        unsafe { std::env::set_var("NEIGE_WAVE_TASK_BUDGET", v) };
    }
    fn remove() {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var("NEIGE_WAVE_TASK_BUDGET") };
    }

    remove();
    assert_eq!(Scheduler::budget_from_env(1), 1, "unset → default 1");
    set("");
    assert_eq!(Scheduler::budget_from_env(1), 1, "empty → default");
    set("nope");
    assert_eq!(Scheduler::budget_from_env(1), 1, "garbage → default");
    set("0");
    assert_eq!(Scheduler::budget_from_env(1), 1, "zero → default");
    set("-2");
    assert_eq!(Scheduler::budget_from_env(1), 1, "negative → default");
    set("3");
    assert_eq!(Scheduler::budget_from_env(1), 3, "valid → override");

    match saved {
        Some(v) => set(&v),
        None => remove(),
    }
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

// ------------------------------------------------- payload determinism

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
        "wave_id": "w",
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
        "wave_id": "w",
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
fn budget_greater_than_one_relies_on_claim_time_workspace_leases() {
    let tasks = vec![
        task("a", TaskStatus::Pending, &[], 0),
        task("b", TaskStatus::Pending, &[], 0),
    ];
    let ready = compute_ready(&tasks, 2);
    assert_eq!(keys(&ready), vec!["a", "b"]);
    // There is intentionally no cwd/resource collision check here:
    // Codex claims acquire `.claude/worktrees/<wave>/<card>` leases,
    // and card ids make those paths structurally disjoint.
}

#[test]
fn terminal_payload_without_cwd_keeps_row_none() {
    // #644 followup: a terminal row with `cwd = NULL` must produce
    // `cwd: null` in the payload — the row value, NOT a materialized
    // `default_cwd()` (HOME/current dir). Anything env-derived here
    // would change `stable_payload_hash` across an env-changing
    // restart, making `resume_dispatched` classify its OWN operation
    // as a permanent foreign idempotency conflict and fail the task
    // instead of recovering it. `cwd: null` is by construction
    // independent of process env (no env value can be JSON null);
    // the adapter resolves the default at spawn time instead.
    let mut terminal = task("t", TaskStatus::Dispatched, &[], 0);
    terminal.kind = TaskKind::Terminal;
    terminal.goal = "make test".into();
    terminal.cwd = None;
    let (kind, p1) = build_worker_payload(&terminal).unwrap();
    assert_eq!(kind, "terminal-worker");
    assert_eq!(p1["cwd"], Value::Null, "row None stays None");
    // Restart simulation: rebuild from the same frozen row → the
    // payload and its idempotency hash must be byte-identical.
    let (_, p2) = build_worker_payload(&terminal).unwrap();
    assert_eq!(p1, p2);
    assert_eq!(
        stable_payload_hash(&p1).unwrap(),
        stable_payload_hash(&p2).unwrap()
    );
}

// ----------------------------------------------------- inflight guard

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
    let cove = repo
        .cove_create(crate::model::NewCove {
            name: "claude-timeout".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "claude-timeout".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");

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
        wave.id.clone(),
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
    running.id = format!("{}:claude-timeout", wave.id.as_str());
    running.wave_id = wave.id.as_str().to_string();
    running.kind = TaskKind::Claude;
    running.worker_card_id = Some(card.id.to_string());
    running.running_deadline_ms = Some(now - 1);
    running.created_at_ms = now;
    running.updated_at_ms = now;
    let task_id = running.id.clone();
    calm_truth::db::sqlite::task_insert_tx(&mut tx, &running)
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
                   lease_id, card_id, wave_id, path, state, lease_owner, lease_until_ms,
                   boot_id, created_at_ms, updated_at_ms
               )
               VALUES ('lease-claude-timeout', ?1, ?2, '/tmp/neige-claude-timeout-lease',
                       'held', 'test-owner', ?3, NULL, ?4, ?4)"#,
    )
    .bind(card.id.as_ref())
    .bind(wave.id.as_str())
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert held lease");

    let events = EventBus::new();
    let write = WriteContext::new(
        concrete.card_role_cache().clone(),
        concrete.wave_cove_cache().clone(),
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
        Arc::new(Semaphore::new(1)),
    );
    scheduler.mark_boot_sweep_complete();

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
    let cove = repo
        .cove_create(crate::model::NewCove {
            name: "timeout-race".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "timeout-race".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let mut stored = task("race", TaskStatus::Done, &[], 0);
    stored.id = format!("{}:race", wave.id.as_str());
    stored.wave_id = wave.id.as_str().to_string();
    stored.worker_card_id = Some("card-race".into());
    let mut snapshot = stored.clone();
    snapshot.status = TaskStatus::Running;
    snapshot.running_deadline_ms = Some(now_ms() - 1);

    let pool = concrete.pool().clone();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool)
        .await
        .expect("begin task tx");
    calm_truth::db::sqlite::task_insert_tx(&mut tx, &stored)
        .await
        .expect("insert done task");
    tx.commit().await.expect("commit task tx");

    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner, lease_until_ms,
                   boot_id, created_at_ms, updated_at_ms
               )
               VALUES ('lease-race', 'card-race', ?1, '/tmp/neige-timeout-race',
                       'held', 'test-owner', ?2, NULL, ?3, ?3)"#,
    )
    .bind(wave.id.as_str())
    .bind(now + 60_000)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert held lease");
    sqlx::query(
        r#"INSERT INTO worker_sessions (
                   id, wave_id, provider, mode, contract, state, card_id,
                   created_at_ms, updated_at_ms
               )
               VALUES ('runtime-race', ?1, 'codex', 'resumable', 'executor',
                       'running', 'card-race', ?2, ?2)"#,
    )
    .bind(wave.id.as_str())
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert worker session");

    let events = EventBus::new();
    let write = WriteContext::new(
        concrete.card_role_cache().clone(),
        concrete.wave_cove_cache().clone(),
    );
    let scheduler = Scheduler::new(
        repo,
        events,
        write,
        Weak::<OperationRuntime>::new(),
        Arc::new(Semaphore::new(1)),
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
