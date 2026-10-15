//! Settled attempt ownership, rather than track closure, ends PTY workers.
use super::*;
use calm_types::task_recovery::{TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint};

async fn attempt(
    h: &Harness,
    p: &PtySession,
    key: &str,
    number: i64,
    status: &str,
    age: Option<i64>,
) -> String {
    let id = format!("{}:{key}:{number}", h.track);
    if number > 1 {
        let previous = format!("{}:{key}:{}", h.track, number - 1);
        let origin = TaskAttemptOrigin::Recovery {
            previous_attempt_id: previous,
            idempotency_key: "retry".into(),
            request_fingerprint: "retry".into(),
            reason: "retry".into(),
            actor: calm_server::ids::ActorId::User,
            constraint: TaskRecoveryConstraint::V1 {
                refs: vec![calm_types::event::TaskContextRef {
                    track_id: h.track.clone().into(),
                    block_id: "test".into(),
                    rev: 1,
                    hash: "0".repeat(64),
                    is_root: true,
                }],
                spawn: TASK_IN_TRACK_ROUTE.into(),
                declared_by: "user".into(),
            },
        };
        sqlx::query(concat!(
            "INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,",
            "origin_json,created_at_ms) VALUES (?1,?2,?3,?4,?5,0)"
        ))
        .bind(&id)
        .bind(&h.track)
        .bind(key)
        .bind(number)
        .bind(serde_json::to_string(&origin).unwrap())
        .execute(h.sql.pool())
        .await
        .unwrap();
    }
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,",
        "declared_by,created_at_ms,updated_at_ms,finished_at_ms) ",
        "VALUES (?1,?2,?3,'terminal','test','[]',?4,?5,'user',?6,?6,?7)"
    ))
    .bind(&id)
    .bind(&h.track)
    .bind(key)
    .bind(status)
    .bind(&p.card)
    .bind(calm_server::model::now_ms())
    .bind(age.map(|age| calm_server::model::now_ms() - age))
    .execute(h.sql.pool())
    .await
    .unwrap();
    id
}

async fn assert_reaped(h: &Harness, p: &PtySession) {
    let (state, completed, _) = session_row(h, &p.session).await;
    assert_eq!(state, "exited");
    assert!(completed.is_some());
    await_terminal_exit(h, &p.terminal).await;
    await_pid_gone(p.pid).await;
    assert!(h.state.terminal_renderer.get(&p.terminal).is_none());
}

#[tokio::test]
async fn worker_reap_settled_then_reaped_open_and_closed() {
    for closed in [false, true] {
        for status in ["done", "failed", "canceled"] {
            let h = Harness::start().await;
            let p = open_sleeper(&h, "settled").await;
            sqlx::query("UPDATE worker_sessions SET provider='claude' WHERE id=?1")
                .bind(&p.session)
                .execute(h.sql.pool())
                .await
                .unwrap();
            attempt(&h, &p, "settled", 1, status, Some(120_000)).await;
            if closed {
                complete_track(&h, &h.track).await;
            }
            assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);
            calm_server::terminal_sweeper::sweep(&h.state)
                .await
                .unwrap();
            assert_reaped(&h, &p).await;
            let before = write_snapshot(&h, &p).await;
            calm_server::terminal_sweeper::sweep(&h.state)
                .await
                .unwrap();
            assert_eq!(write_snapshot(&h, &p).await, before);
            h.stop(&p.terminal).await;
        }
    }
}

#[tokio::test]
async fn worker_reap_in_flight_not_reaped_even_with_settled_attempt_on_card() {
    for status in ["dispatched", "running", "verifying"] {
        let h = Harness::start().await;
        let p = open_sleeper(&h, "in-flight").await;
        attempt(&h, &p, "old", 1, "done", Some(120_000)).await;
        attempt(&h, &p, "active", 1, status, None).await;
        complete_track(&h, &h.track).await;
        let before = write_snapshot(&h, &p).await;
        assert!(sweep_set(&h).await.is_empty());
        calm_server::terminal_sweeper::sweep(&h.state)
            .await
            .unwrap();
        assert_eq!(write_snapshot(&h, &p).await, before);
        assert!(pid_alive(p.pid));
        h.stop(&p.terminal).await;
    }
}

#[tokio::test]
async fn worker_reap_retry_on_another_card_preserves_new_attempt() {
    let h = Harness::start().await;
    let old = open_sleeper(&h, "old").await;
    let new = open_sleeper(&h, "new").await;
    attempt(&h, &old, "retry", 1, "failed", Some(120_000)).await;
    attempt(&h, &new, "retry", 2, "running", None).await;
    assert_eq!(sweep_set(&h).await, vec![old.session.clone()]);
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    assert_reaped(&h, &old).await;
    assert_eq!(session_row(&h, &new.session).await.0, "running");
    assert!(pid_alive(new.pid));
    h.stop(&new.terminal).await;
}

#[tokio::test]
async fn worker_reap_within_grace_or_missing_finish_not_reaped() {
    for age in [Some(0), Some(30_000), None] {
        let h = Harness::start().await;
        let p = open_sleeper(&h, "grace").await;
        attempt(&h, &p, "grace", 1, "done", age).await;
        complete_track(&h, &h.track).await;
        let before = write_snapshot(&h, &p).await;
        assert!(sweep_set(&h).await.is_empty());
        calm_server::terminal_sweeper::sweep(&h.state)
            .await
            .unwrap();
        assert_eq!(write_snapshot(&h, &p).await, before);
        assert!(pid_alive(p.pid));
        h.stop(&p.terminal).await;
    }
}

#[tokio::test]
async fn worker_reap_dead_pty_running_converged() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "dead").await;
    // A resumable Codex worker retains live session authority after PTY exit.
    sqlx::query("UPDATE worker_sessions SET provider='codex', mode='resumable' WHERE id=?1")
        .bind(&p.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let term = h
        .state
        .repo
        .terminal_get(&p.terminal)
        .await
        .unwrap()
        .unwrap();
    calm_server::terminal_sweeper::reap_terminal_artifacts(&h.state, &term).await;
    await_terminal_exit(&h, &p.terminal).await;
    await_pid_gone(p.pid).await;
    assert_eq!(session_row(&h, &p.session).await.0, "running");
    attempt(&h, &p, "dead", 1, "done", Some(120_000)).await;
    assert_eq!(sweep_set(&h).await, vec![p.session.clone()]);
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    assert_eq!(session_row(&h, &p.session).await.0, "exited");
    h.stop(&p.terminal).await;
}

#[tokio::test]
async fn worker_reap_user_terminal_and_planner_untouched() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "user").await;
    complete_track(&h, &h.track).await;
    let before = write_snapshot(&h, &p).await;
    assert!(sweep_set(&h).await.is_empty());
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    assert_eq!(write_snapshot(&h, &p).await, before);
    assert!(pid_alive(p.pid));
    assert_eq!(session_row(&h, &h.session_id).await.0, "starting");
    h.stop(&p.terminal).await;
}

#[tokio::test]
async fn worker_reap_claim_rechecks_in_flight_before_write() {
    let h = Harness::start_with_fake_codex().await;
    let p = open_sleeper(&h, "claim").await;
    make_codex_with_active_turn(&h, &p).await;
    attempt(&h, &p, "claim", 1, "done", Some(120_000)).await;
    let candidate = sole_candidate(&h).await;
    calm_server::terminal_sweeper::end_settled_worker_session_before_write_for_test(
        &h.state,
        &candidate,
        || async {
            attempt(&h, &p, "racing", 1, "verifying", None).await;
        },
    )
    .await
    .unwrap();
    assert_eq!(session_row(&h, &p.session).await.0, "running");
    assert!(
        h.state
            .shared_codex_appserver
            .interrupted_turns_for_test()
            .is_empty()
    );
    assert!(pid_alive(p.pid));
    h.stop(&p.terminal).await;
}

#[tokio::test]
async fn worker_reap_codex_interrupt_after_claim() {
    let h = Harness::start_with_fake_codex().await;
    let p = open_sleeper(&h, "codex").await;
    make_codex_with_active_turn(&h, &p).await;
    attempt(&h, &p, "codex", 1, "done", Some(120_000)).await;
    let candidate = sole_candidate(&h).await;
    calm_server::terminal_sweeper::end_settled_worker_session_before_write_for_test(
        &h.state,
        &candidate,
        || async {
            assert!(
                h.state
                    .shared_codex_appserver
                    .interrupted_turns_for_test()
                    .is_empty()
            );
        },
    )
    .await
    .unwrap();
    assert_reaped(&h, &p).await;
    assert_eq!(
        h.state.shared_codex_appserver.interrupted_turns_for_test(),
        vec![("t-1".to_owned(), "turn-1".to_owned())]
    );
    h.stop(&p.terminal).await;
}

#[tokio::test]
async fn worker_reap_reattach_after_registry_reset() {
    let h = Harness::start().await;
    let p = open_sleeper(&h, "registry-reset").await;
    attempt(&h, &p, "registry", 1, "done", Some(120_000)).await;
    h.state
        .terminal_renderer
        .get(&p.terminal)
        .unwrap()
        .disconnect_output_source_for_test();
    let repo: Arc<dyn calm_server::db::Repo> = h.sql.clone();
    let fresh = calm_server::state::AppState::from_parts(
        repo,
        h.state.events.clone(),
        h.state.daemon.clone(),
        h.state.plugin.clone(),
        h.state.codex.clone(),
        Some(h.state.card_role_cache.clone()),
        Some(h.state.track_area_cache.clone()),
    );
    calm_server::terminal_sweeper::sweep(&fresh).await.unwrap();
    assert_eq!(session_row(&h, &p.session).await.0, "exited");
    await_terminal_exit(&h, &p.terminal).await;
    await_pid_gone(p.pid).await;
    assert!(fresh.terminal_renderer.get(&p.terminal).is_none());
    h.stop(&p.terminal).await;
}

/// A harness row is outside the set by two independent conditions — never `running`, no `terminal_run_id` — so no single-factor change admits it.
#[tokio::test]
async fn worker_reap_planner_harness_untouched() {
    use calm_server::db::sqlite::{card_create_with_id_tx, session_start_runtime_tx};
    use calm_server::harness::HarnessSnapshot;
    use calm_server::model::NewCard;
    use calm_server::model::NewTrack;
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let h = Harness::start().await;
    // One planner-role card per track: the harness track already has one,
    // so the idle harness planner gets its own track (same area).
    let track = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: h.area_id.clone().into(),
            title: "harness-set".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string();
    let planner_card = new_id();
    let planner_ws = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        planner_card.clone(),
        NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Planner,
        true,
        &h.state.card_role_cache,
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: planner_ws.clone(),
            card_id: planner_card.clone(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("th-planner".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(
                serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
            ),
            spawn_op_id: None,
            now_ms: 1_000,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    complete_track(&h, &track).await;

    // Both excluding facts hold on the row itself.
    let (state, terminal_run_id, mode): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, terminal_run_id, json_extract(handle_state_json, '$.mode') \
           FROM worker_sessions WHERE id = ?1",
    )
    .bind(&planner_ws)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(mode.as_deref(), Some("harness"));
    assert_ne!(
        state, "running",
        "a harness row is idle / turn_pending, never `running`"
    );
    assert_eq!(terminal_run_id, None, "a harness row has no PTY");
    assert!(
        sweep_set(&h).await.is_empty(),
        "structurally outside the set"
    );

    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    let (state, completed_at, _) = session_row(&h, &planner_ws).await;
    assert_eq!(
        state, "idle",
        "the planner can still be asked on a closed track"
    );
    assert_eq!(completed_at, None);
    // The harness' own Planner card session (`starting`, PTY-backed) is
    // outside the set as well.
    let (state, _, _) = session_row(&h, &h.session_id).await;
    assert_eq!(state, "starting");
}
