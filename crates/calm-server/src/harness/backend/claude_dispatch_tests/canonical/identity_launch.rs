//! #3318: the launch record's ownership, staleness and persistence failures. Only durable
//! facts decide whether a `--session-id` launch may run; every failure launches nothing.
use super::identity_recovery::binding;
use super::*;

async fn launch(
    fx: &Fixture,
) -> Option<crate::db::sqlite::claude_session_launch::ClaudeSessionLaunch> {
    let mut conn = fx.owner.repo.pool().acquire().await.unwrap();
    crate::db::sqlite::claude_session_launch::get(&mut conn, &fx.owner.worker)
        .await
        .unwrap()
}

async fn seed_launch(fx: &Fixture, card: &str, track: &str, thread: &str) {
    sqlx::query(
        "INSERT INTO claude_session_launches
        (worker_session_id, card_id, track_id, thread_id, execution_id, created_at_ms)
        VALUES (?, ?, ?, ?, 'earlier-execution', 0)",
    )
    .bind(&fx.owner.worker)
    .bind(card)
    .bind(track)
    .bind(thread)
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
}

async fn fail_launch_writes(fx: &Fixture, operation: &str) {
    sqlx::query(&format!(
        "CREATE TRIGGER fail_launch_{operation} BEFORE {operation} ON claude_session_launches
        BEGIN SELECT RAISE(ABORT,'launch record {operation} failed'); END"
    ))
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
}

async fn drop_trigger(fx: &Fixture, operation: &str) {
    sqlx::query(&format!("DROP TRIGGER fail_launch_{operation}"))
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
}

/// A refused start sends nothing and records the original as not dispatched.
async fn assert_not_dispatched(fx: &Fixture) {
    if let Ok(report) = fx.dispatch().await {
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::NotDispatched(_)
        ));
    }
    assert!(
        !fx.dir.path().join("spawns").exists(),
        "nothing was launched"
    );
    fx.no_stdin();
    assert!(matches!(
        fx.checkpoint().await.attempts[0].delivery,
        InputDeliveryFact::NotDispatched { .. }
    ));
    assert_eq!(binding(fx).await, None);
}

#[tokio::test]
async fn canonical_claude_identity_launch_record_write_failure_launches_nothing() {
    let fx = Fixture::persistent("exit").await;
    fail_launch_writes(&fx, "INSERT").await;
    assert_not_dispatched(&fx).await;
    assert_eq!(launch(&fx).await, None);
    fx.session.shutdown().await.unwrap();
}

/// The record commits with the launch's admission: a refused admission records nothing,
/// so it never holds a carrier whose process never started.
#[tokio::test]
async fn canonical_claude_identity_launch_refused_admission_leaves_no_record() {
    let fx = Fixture::persistent("exit").await;
    sqlx::query(
        "CREATE TRIGGER fail_admission BEFORE INSERT ON workspace_executions
        BEGIN SELECT RAISE(FAIL,'admission blocked'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_not_dispatched(&fx).await;
    assert_eq!(launch(&fx).await, None);
    assert_eq!(fx.backend.issuance_hold(), None);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_launch_crossing_its_owner_refuses_and_holds() {
    for crossed in ["card", "track"] {
        let fx = Fixture::persistent("exit").await;
        let (card, track) = match crossed {
            "card" => ("another-card", fx.owner.track.as_str()),
            _ => (fx.owner.card.as_str(), "another-track"),
        };
        seed_launch(&fx, card, track, &fx.owner.thread).await;
        assert_not_dispatched(&fx).await;
        assert!(launch(&fx).await.is_some(), "{crossed}: evidence is kept");
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        assert!(
            crate::claude_planner::recovery::identity_unconfirmed_tx(&mut tx, &fx.owner.worker)
                .await
                .unwrap(),
            "{crossed}: crossed evidence is held"
        );
        tx.commit().await.unwrap();
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_identity_launch_for_another_thread_is_stale() {
    let fx = Fixture::persistent("exit").await;
    let other = uuid::Uuid::new_v4().to_string();
    seed_launch(&fx, &fx.owner.card, &fx.owner.track, &other).await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    assert!(
        !crate::claude_planner::recovery::identity_unconfirmed_tx(&mut tx, &fx.owner.worker)
            .await
            .unwrap(),
        "another thread's launch says nothing about this one"
    );
    tx.commit().await.unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
    assert!(argv.lines().any(|arg| arg == "--session-id"), "{argv}");
    assert_eq!(
        binding(&fx).await.as_deref(),
        Some(fx.owner.thread.as_str())
    );
    assert_eq!(launch(&fx).await, None, "the bind retired this launch");
    fx.session.shutdown().await.unwrap();
}

/// The bind and the retirement of its launch commit together: a failed delete leaves
/// neither, and only verified replay evidence repairs the binding after a restart.
#[tokio::test]
async fn canonical_claude_identity_launch_delete_failure_keeps_the_bind_unpersisted() {
    let fx = Fixture::persistent("exit").await;
    fail_launch_writes(&fx, "DELETE").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    assert_eq!(
        binding(&fx).await,
        None,
        "the bind rolled back with its delete"
    );
    let recorded = launch(&fx).await.expect("the launch stays recorded");
    assert_eq!(recorded.thread_id, fx.owner.thread);
    fx.session.shutdown().await.unwrap();
    drop_trigger(&fx, "DELETE").await;
    let spawns = std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap();
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    assert_eq!(
        binding(&fx).await.as_deref(),
        Some(fx.owner.thread.as_str())
    );
    assert_eq!(launch(&fx).await, None);
    assert_eq!(backend.issuance_hold(), None);
    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap(),
        spawns
    );
    session.shutdown().await.unwrap();
}

async fn held(fx: &Fixture) -> bool {
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let held = crate::claude_planner::recovery::identity_unconfirmed_tx(&mut tx, &fx.owner.worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    held
}

/// Fail the claim's release at one transition: `Releasing` before any payload byte, or
/// `Released` after the whole payload reached the process.
async fn fail_release(fx: &Fixture, to: &str) {
    sqlx::query(&format!(
        "CREATE TRIGGER fail_release BEFORE UPDATE OF provider_execution_json ON worker_sessions
        WHEN NEW.provider_execution_json LIKE '%\"{to}\"%'
        BEGIN SELECT RAISE(ABORT,'release to {to} failed'); END"
    ))
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
}

/// A release refused while the claim is still held never ran Claude: the launch is retired
/// by its execution, so the carrier is not held and a reopened session launches anew.
#[tokio::test]
async fn canonical_claude_identity_launch_refused_before_release_is_retired() {
    let mut fx = Fixture::persistent("exit").await;
    fail_release(&fx, "Releasing").await;
    assert_not_dispatched(&fx).await;
    assert_eq!(launch(&fx).await, None, "an unreleased launch is retired");
    assert!(!held(&fx).await);
    fx.session.shutdown().await.unwrap();
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    fx.backend = backend;
    fx.session = session;
    assert_eq!(fx.backend.issuance_hold(), None);
    fx.session.shutdown().await.unwrap();
}

/// A launcher that exits while it still waits for its release never ran Claude: the release
/// finds it gone with the claim still held, and the launch is retired by its execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_claude_identity_launch_exited_before_release_is_retired() {
    let fx = Fixture::new("exit").await;
    let claude = fx.dir.path().join("claude").display().to_string();
    fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
        let output = std::process::Command::new("pgrep")
            .args(["-f", &claude])
            .output()
            .unwrap();
        let pids = String::from_utf8(output.stdout).unwrap();
        let pids = pids.split_whitespace().collect::<Vec<_>>();
        assert!(!pids.is_empty(), "the launcher is held");
        std::process::Command::new("kill")
            .arg("-KILL")
            .args(&pids)
            .status()
            .unwrap();
        // Reaped, so the Engine has observed the exit before the release looks.
        for pid in pids {
            let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
            while proc.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }));
    let _ = fx.dispatch().await;
    assert_eq!(fx.session.user_line_writes_for_test(), 0);
    assert!(!fx.dir.path().join("spawns").exists(), "Claude never ran");
    assert_eq!(launch(&fx).await, None, "an unreleased launch is retired");
    assert!(!held(&fx).await);
    fx.session.shutdown().await.unwrap();
}

/// Once the release payload reached the process, Claude may have run with `--session-id`; a
/// later refusal proves nothing, so the launch is kept and held.
#[tokio::test]
async fn canonical_claude_identity_launch_refused_after_release_is_kept() {
    let fx = Fixture::persistent("exit").await;
    fail_release(&fx, "Released").await;
    let _ = fx.dispatch().await;
    fx.no_stdin();
    assert!(launch(&fx).await.is_some(), "a released launch is kept");
    assert!(held(&fx).await);
    fx.session.shutdown().await.unwrap();
}

/// A pre-send refusal after the release wrote no user byte, but Claude already ran with
/// `--session-id`; no Neige-owned fact proves it created no session, so the launch is kept.
#[tokio::test]
async fn canonical_claude_identity_launch_refused_before_write_after_release_is_kept() {
    let fx = Fixture::new("exit").await;
    let (auth, login) = (fx.auth.clone(), fx.login.clone());
    fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
        auth.sessions.remove(&login).unwrap();
    }));
    let _ = fx.dispatch().await;
    fx.no_stdin();
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    assert!(launch(&fx).await.is_some(), "a released launch is kept");
    assert!(held(&fx).await);
    fx.session.shutdown().await.unwrap();
}

/// A user line that crossed to the CLI before its write failed may have created the session:
/// the launch is kept and held.
#[tokio::test]
async fn canonical_claude_identity_launch_crossed_write_is_kept() {
    let fx = Fixture::new("exit").await;
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.fail_user_flush_for_test(pause.clone());
    pause.release.notify_one();
    let _ = fx.dispatch().await;
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    assert!(launch(&fx).await.is_some(), "a crossed launch is kept");
    assert!(held(&fx).await);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_bound_row_retires_matching_launch_on_open() {
    let fx = Fixture::persistent("exit").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
    seed_launch(&fx, &fx.owner.card, &fx.owner.track, &fx.owner.thread).await;
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    assert_eq!(backend.issuance_hold(), None);
    assert_eq!(
        launch(&fx).await,
        None,
        "a bound session leaves no unconfirmed record"
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_stale_completion_cannot_retire_newer_launch() {
    let fx = Fixture::persistent("exit").await;
    seed_launch(&fx, &fx.owner.card, &fx.owner.track, &fx.owner.thread).await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    assert!(
        !crate::db::sqlite::claude_session_launch::retire(
            &mut tx,
            &fx.owner.worker,
            "stale-execution"
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    assert_eq!(launch(&fx).await.unwrap().execution_id, "earlier-execution");
    fx.session.shutdown().await.unwrap();
}
