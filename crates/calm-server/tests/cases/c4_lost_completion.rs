//! Lost completion must reconcile the exact retained driver and its durable session facts.
use super::claude_planner_stack_fixture::{Root, Stack};
use axum::http::StatusCode;
use calm_server::{
    provider_execution as owner,
    test_seams::{PausePoint, install_pause_for_test},
};
use calm_truth::db::sqlite::provider_execution as sql;
use std::{sync::Arc, time::Duration};

async fn bound(stack: &Stack, session: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = stack
                .repo()
                .session_get(&session.to_owned().into())
                .await
                .unwrap()
                .unwrap();
            if row.agent_session_id.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn sweep_clear(
    stack: &Stack,
    session: &calm_server::claude_planner::session::ClaudePlannerSession,
    worker: &str,
) {
    let pool = stack.repo().sqlite_db().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            session
                .host()
                .provider_execution
                .sweep(&pool, session.host().execution.engine())
                .await
                .unwrap();
            if sql::load(pool.read_pool(), worker).await.unwrap().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_lost_completion_retains_claim_until_aborted_driver_join_resolves() {
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let (status, body) = stack.post_input(&card, "abort while finishing").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    bound(&stack, &runtime.id).await;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    let pool = stack.repo().sqlite_db().unwrap();
    let claim = sql::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    let id = owner::execution_id(claim.record()).unwrap();
    let pause = PausePoint::default();
    install_pause_for_test("c4-driver-dropped", id.as_str(), pause.clone());
    session.abort_driver_for_test().await;
    tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
        .await
        .unwrap();
    let registered = &session.host().provider_execution;
    assert!(!registered.drivers.contains(&id));
    registered
        .stop(
            &pool,
            session.host().execution.engine(),
            &runtime.id,
            &claim,
            owner::StopReason::DriverLost,
        )
        .await
        .unwrap();
    owner::wait_settled(
        session.host().execution.engine().unwrap(),
        &id,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    registered
        .sweep(&pool, session.host().execution.engine())
        .await
        .unwrap();
    let retained = sql::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .is_some();
    // Release even on the unfixed tree, so a red assertion cannot strand task destruction.
    pause.release.notify_one();
    assert!(
        retained,
        "completion must retain the claim while the exact join is unresolved"
    );
    sweep_clear(&stack, &session, &runtime.id).await;
    assert_eq!(
        session.active_turn_id_for_thread(runtime.thread_id.as_ref().unwrap()),
        None
    );
    stack.run_turn(&root, &card, "exit", "slot released").await;
    stack.shutdown().await;
}

#[tokio::test]
async fn c4_lost_completion_recovers_first_bind_after_abort() {
    recover_first_bind(false, false).await;
}

#[tokio::test]
async fn c4_lost_completion_recovers_first_bind_after_known_completion_failure() {
    recover_first_bind(true, false).await;
}

#[tokio::test]
async fn c4_lost_completion_recovers_timed_out_bind_after_abort() {
    recover_first_bind(false, true).await;
}

#[tokio::test]
async fn c4_lost_completion_recovers_timed_out_bind_after_known_completion_failure() {
    recover_first_bind(true, true).await;
}

async fn recover_first_bind(known_failure: bool, bind_timeout: bool) {
    let root = Root::new(if known_failure { "exit" } else { "hold" });
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card).await;
    let protocol = PausePoint::default();
    let bind = PausePoint::default();
    if bind_timeout {
        install_pause_for_test("c4-bind-session", &runtime.id, bind.clone());
    }
    if known_failure {
        install_pause_for_test("c4-protocol-ended", &runtime.id, protocol.clone());
    }
    let (status, body) = stack.post_input(&card, "bind before loss").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    if bind_timeout {
        tokio::time::timeout(Duration::from_secs(15), bind.entered.notified())
            .await
            .unwrap();
        // The real driver's bounded bind must expire; a translated item proves it
        // moved past init without persisting. Do not release the bind pause.
        stack.wait_phase(&runtime.id, "turn_running").await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let rows = stack
                    .repo()
                    .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
                    .await
                    .unwrap();
                if rows.iter().any(|row| row.method == "item/started") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            stack
                .repo()
                .session_get(&runtime.id.clone().into())
                .await
                .unwrap()
                .unwrap()
                .agent_session_id
                .is_none()
        );
    } else {
        bound(&stack, &runtime.id).await;
    }
    if known_failure {
        tokio::time::timeout(Duration::from_secs(15), protocol.entered.notified())
            .await
            .unwrap();
        // Known completion records the outcome, then fails removing its instructions.
        let dir = session.host().instructions_dir.clone();
        let mut damaged = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.path().is_file() {
                let bytes = std::fs::read(entry.path()).unwrap();
                std::fs::remove_file(entry.path()).unwrap();
                std::fs::create_dir(entry.path()).unwrap();
                damaged.push((entry.path(), bytes));
            }
        }
        assert!(!damaged.is_empty());
        protocol.release.notify_one();
        assert!(!session.wait_driver_for_test().await);
        for (path, bytes) in damaged {
            std::fs::remove_dir(&path).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    } else {
        session.abort_driver_for_test().await;
        assert!(!session.wait_driver_for_test().await);
    }
    sweep_clear(&stack, &session, &runtime.id).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
    assert!(Arc::ptr_eq(
        &session,
        &stack
            .harness(&runtime.id)
            .claude_session_for_test()
            .unwrap()
    ));
    // Compact immediately: a subsequent successful work turn would mask the lost bind.
    root.set_scenario("compact");
    let (status, body) = stack
        .send("POST", &format!("/api/cards/{card}/planner/compact"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.wait_provider_clear(&runtime.id).await;
    bound(&stack, &runtime.id).await;
    let compact_argv = root.read_fake("argv").unwrap();
    assert!(
        compact_argv.lines().any(|arg| arg == "--resume"),
        "{compact_argv}"
    );
    assert!(
        !compact_argv.lines().any(|arg| arg == "--session-id"),
        "{compact_argv}"
    );
    stack
        .run_turn(&root, &card, "exit", "resume the same handle")
        .await;
    assert_eq!(
        stack
            .harness(&runtime.id)
            .snapshot()
            .await
            .unwrap()
            .token_usage
            .unwrap()
            .total_tokens,
        if known_failure { 30 } else { 15 },
        "lost completion must preserve the lifetime usage total"
    );
    let argv = root.read_fake("argv").unwrap();
    assert!(argv.lines().any(|arg| arg == "--resume"), "{argv}");
    assert!(!argv.lines().any(|arg| arg == "--session-id"), "{argv}");
    stack.shutdown().await;
}

#[tokio::test]
async fn c4_lost_completion_does_not_invent_an_absent_binding() {
    let root = Root::new("stall");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let (status, body) = stack.post_input(&card, "never initialized").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    stack.wait_phase(&runtime.id, "turn_running").await;
    session.abort_driver_for_test().await;
    assert!(!session.wait_driver_for_test().await);
    sweep_clear(&stack, &session, &runtime.id).await;
    let row = stack
        .repo()
        .session_get(&runtime.id.clone().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.agent_session_id, None);
    stack.wait_phase(&runtime.id, "turn_completed").await;
    stack
        .run_turn(&root, &card, "exit", "first actual session")
        .await;
    let argv = root.read_fake("argv").unwrap();
    assert!(argv.lines().any(|arg| arg == "--session-id"), "{argv}");
    stack.shutdown().await;
}
