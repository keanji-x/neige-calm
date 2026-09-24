//! #1791 PR2b: how a Claude Planner turn stops and what it leaves on its worker-session row —
//! the interrupt's stop timer, a stop that meets a ready result, a CLI that stops reading stdin or
//! never stops writing, the `agent_session_id` bind and resume. Rig: `claude_planner_session_fixture.rs`.

use std::time::{Duration, Instant};

use super::claude_planner_session_fixture::{
    Rig, client_id, completed_turn, until_completed, wait_for_file,
};

/// The harness's interrupt budget less the stop's own worst case leaves this for settlement to
/// begin; every stop test must settle inside it.
const SETTLE_BUDGET: Duration = Duration::from_secs(25);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ignored_interrupt_is_ended_by_the_stop_timer_within_budget() {
    let rig = Rig::new("ignore-interrupt").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    let interrupted_at = Instant::now();
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled inside the 25 s interrupt budget");
    let elapsed = interrupted_at.elapsed();

    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    assert!(
        elapsed >= Duration::from_secs(9),
        "the timer, not the CLI, ended it: {elapsed:?}"
    );
    assert_eq!(rig.outcomes().await[0]["status"], "interrupted");
    assert!(
        rig.marked_pids().is_empty(),
        "no marked process after the timer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdout_that_is_not_utf8_fails_the_turn_as_protocol() {
    let rig = Rig::new("invalid-utf8").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert_eq!(completed["status"], "failed");
    let message = completed["error"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("protocol"), "{message}");
    assert!(rig.marked_pids().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_init_binds_the_row_and_a_reopened_session_resumes() {
    let rig = Rig::new("exit").await;
    assert_eq!(rig.agent_session_id().await, None);
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    until_completed(&mut rx).await;
    assert_eq!(
        rig.agent_session_id().await.as_deref(),
        Some(rig.thread.as_str())
    );

    let reopened = rig.open_session().await;
    let mut rx = reopened.subscribe_notifications();
    reopened
        .turn_start(&rig.thread, rig.text("after a restart"), &client_id())
        .await
        .expect("turn_start");
    until_completed(&mut rx).await;
    let argv = rig.read_bin("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|a| *a == "--resume").expect("resume");
    assert_eq!(argv[at + 1], rig.thread);
}

/// A shutdown that fires while the CLI's result already waits on stdout: the session is stuck
/// writing an answer into a full stdin pipe when the result arrives, so the stop and the ready
/// result line meet in one poll. The result came first, so the turn is completed (§6.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ready_result_wins_over_a_stop_that_fires_with_it() {
    let rig = Rig::new("flood").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("emitted")).await;
    rig.session().shutdown().await.expect("shutdown stops");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert_eq!(completed["id"], turn.as_str());
    assert_eq!(completed["status"], "completed");
    assert_eq!(rig.outcomes().await[0]["status"], "completed");
    assert!(rig.marked_pids().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_token_installs_once() {
    let rig = Rig::new("exit").await;
    assert!(rig.session().install_mcp_token("second".into()).is_err());
}

/// A CLI that stopped reading stdin and keeps sending control requests: neither the interrupt's
/// own write nor the unanswerable requests may hold the turn past the stop timer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_cli_that_stopped_reading_stdin_settles_within_budget() {
    let rig = Rig::new("flood").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("emitted")).await;
    let interrupted_at = Instant::now();
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled inside the interrupt budget");

    assert!(interrupted_at.elapsed() <= SETTLE_BUDGET);
    // The result was already written when the stop fired, so it decides the turn.
    assert_eq!(completed_turn(&seen)["status"], "completed");
    assert!(rig.marked_pids().is_empty());
}

/// A CLI that ignores the interrupt and never stops writing: the drain after the stop timer is
/// bounded, so the turn still settles inside the budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cli_that_never_stops_writing_is_stopped_within_budget() {
    let rig = Rig::new("chatty-after-interrupt").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled inside the interrupt budget");

    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    assert!(rig.marked_pids().is_empty());
}

/// The CLI names (and so creates) the session before a later init check fails the turn: the row is
/// bound anyway, and the next spawn resumes instead of reusing `--session-id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_init_check_still_binds_the_session_it_named() {
    let rig = Rig::new("bad-init").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    let completed = completed_turn(&until_completed(&mut rx).await);
    let message = completed["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(message.starts_with("check"), "{message}");
    assert_eq!(
        rig.agent_session_id().await.as_deref(),
        Some(rig.thread.as_str())
    );

    std::fs::write(rig.bin("scenario"), "exit").expect("scenario");
    rig.session()
        .turn_start(&rig.thread, rig.text("again"), &client_id())
        .await
        .expect("turn_start");
    until_completed(&mut rx).await;
    let argv = rig.read_bin("argv").expect("argv");
    assert!(argv.lines().any(|a| a == "--resume"), "{argv}");
    assert!(!argv.lines().any(|a| a == "--session-id"), "{argv}");
}

/// The harness's snapshot owns `active_turn_id`; binding the Claude session must not touch it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bind_leaves_active_turn_id_to_the_harness() {
    let rig = Rig::new("exit").await;
    rig.set_active_turn_id(Some("turn-owned-by-the-run-loop"))
        .await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    until_completed(&mut rx).await;

    assert_eq!(
        rig.agent_session_id().await.as_deref(),
        Some(rig.thread.as_str())
    );
    assert_eq!(
        rig.active_turn_id().await.as_deref(),
        Some("turn-owned-by-the-run-loop")
    );
}

/// The stop timer is armed when the interrupt is asked for, not after its line is written: here
/// stdin is full, so the write waits out its own bound, and the turn (no result to wait for) still
/// settles on the timer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_timer_runs_from_the_interrupt_not_from_its_write() {
    let rig = Rig::new("flood-no-result").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("emitted")).await;
    let interrupted_at = Instant::now();
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled inside the interrupt budget");
    let elapsed = interrupted_at.elapsed();

    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    // 10 s timer + a bounded drain; arming after the 5 s write would land past 15 s.
    assert!(elapsed < Duration::from_secs(13), "{elapsed:?}");
}
