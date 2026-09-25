//! #1791 PR2b: how a Claude Planner turn stops and what it leaves on its worker-session row —
//! the interrupt's stop timer, a stop that meets a ready result, a CLI that stops reading stdin or
//! never stops writing, the `agent_session_id` bind and resume. Rig: `claude_planner_session_fixture.rs`.

use std::time::{Duration, Instant};

use calm_server::claude_planner::session::{SETTLE_AFTER_STOP, STOP_TIMER};

use super::claude_planner_session_fixture::{
    Rig, client_id, completed_turn, until_completed, wait_for_file,
};

/// An interrupt's `TurnCompleted` is due by `settle_by` = the interrupt + [`STOP_TIMER`] +
/// [`SETTLE_AFTER_STOP`]; the tests allow one second of scheduling on top.
const SETTLE_BUDGET: Duration = STOP_TIMER
    .saturating_add(SETTLE_AFTER_STOP)
    .saturating_add(Duration::from_secs(1));

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ignored_interrupt_is_ended_by_the_stop_timer_within_budget() {
    let rig = Rig::new("ignore-interrupt").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .expect("settled by settle_by");
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("after a restart"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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

/// #1791 §5.1 item 2: the credential is minted at the first turn and at most once per harness;
/// the second turn spawns with the same token and the row's hash is unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_token_is_minted_once_at_the_first_turn() {
    let rig = Rig::new("exit").await;
    assert_eq!(
        rig.mcp_token_hash().await,
        None,
        "nothing minted before a turn"
    );
    let mut tokens = Vec::new();
    for text in ["one", "two"] {
        let mut rx = rig.session().subscribe_notifications();
        rig.session()
            .turn_start(&rig.thread, rig.text(text), None, &client_id())
            .await
            .expect("turn_start");
        until_completed(&mut rx).await;
        let env = rig.read_bin("env").expect("env");
        let token = env
            .lines()
            .find_map(|line| line.strip_prefix("NEIGE_MCP_TOKEN="))
            .expect("token")
            .to_string();
        tokens.push(token);
    }
    assert_eq!(tokens[0], tokens[1], "one mint per harness");
    assert_eq!(
        rig.mcp_token_hash().await.as_deref(),
        Some(calm_server::mcp_server::auth::hash_token(&tokens[0]).as_str())
    );
}

/// An install-race loser (never installed) refuses its turn without minting or spawning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_was_never_installed_refuses_without_minting() {
    let rig = Rig::new("exit").await;
    let loser = rig.open_session_uninstalled().await;
    let error = loser
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
        .await
        .expect_err("not installed");
    assert!(error.to_string().contains("not installed"), "{error}");
    assert_eq!(rig.mcp_token_hash().await, None);
    assert!(rig.read_bin("spawns").is_none(), "nothing spawned");
}

/// A CLI that stopped reading stdin and keeps sending control requests: neither the interrupt's
/// own write nor the unanswerable requests may hold the turn past the stop timer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_cli_that_stopped_reading_stdin_settles_within_budget() {
    let rig = Rig::new("flood").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .expect("settled by settle_by");

    let elapsed = interrupted_at.elapsed();
    // Answers are abandoned at the stop deadline and a result read while stopping gets no exit
    // wait, so this settles just after the timer. Abandonment itself is not pinned: without it the
    // in-flight answer adds a random 0-5 s, which `settle_by` still bounds.
    assert!(elapsed < STOP_TIMER + Duration::from_secs(3), "{elapsed:?}");
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled by settle_by");

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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("again"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
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
        .expect("settled by settle_by");
    let elapsed = interrupted_at.elapsed();

    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    // 10 s timer + a bounded drain; arming after the 5 s write would land past 15 s.
    assert!(elapsed < Duration::from_secs(13), "{elapsed:?}");
}

/// A CLI that ignores both the interrupt and SIGTERM and keeps stdout full: with the settlement
/// margin shortened to 3 s, the SIGTERM grace, the drain and the stop are all cut by `settle_by`,
/// and `TurnCompleted` still goes out by then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cli_ignoring_sigterm_still_settles_by_settle_by() {
    let margin = Duration::from_secs(3);
    let rig = Rig::new("stubborn-after-interrupt").await;
    rig.session().set_settle_after_stop_for_test(margin);
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    let interrupted_at = Instant::now();
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let settle_by = STOP_TIMER + margin;
    let seen = tokio::time::timeout(SETTLE_BUDGET, until_completed(&mut rx))
        .await
        .expect("settled");
    let elapsed = interrupted_at.elapsed();

    assert!(
        elapsed < settle_by + Duration::from_millis(750),
        "settled at {elapsed:?}, settle_by was {settle_by:?}"
    );
    assert_eq!(completed_turn(&seen)["status"], "interrupted");
    assert_eq!(rig.outcomes().await[0]["status"], "interrupted");
    assert!(
        rig.marked_pids().is_empty(),
        "the stubborn CLI did not survive"
    );
}

/// #1791 PR4 (§5.1 token invariant): a first-turn mint on a row superseded after the run loop's
/// carrier check returns `Err` and writes nothing, card row included; nothing is spawned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_turn_mint_on_a_superseded_row_fails_and_writes_nothing() {
    let rig = Rig::new("exit").await;
    let mut tx = rig.repo.pool().begin().await.expect("tx");
    calm_server::db::sqlite::session_mark_superseded_runtime_tx(&mut tx, &rig.worker_session_id)
        .await
        .expect("supersede");
    tx.commit().await.expect("commit");

    let error = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
        .await
        .expect_err("the guarded mint refuses a retired row");
    assert!(error.to_string().contains("no longer active"), "{error}");
    assert_eq!(rig.mcp_token_hash().await, None);
    let card_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1")
            .bind(&rig.card_id)
            .fetch_one(rig.repo.pool())
            .await
            .expect("count");
    assert_eq!(
        card_rows, 0,
        "the card write rolled back with the session write"
    );
    assert!(rig.read_bin("spawns").is_none(), "nothing spawned");
}

/// #1791 PR4 (PR2b follow-up): a bind of `agent_session_id` that fails (here cut off by its bound
/// while the database is held) leaves the row unbound; the next turn resumes from memory AND
/// retries the bind, so a later reopen resumes too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_bind_is_retried_on_the_next_turn() {
    let rig = Rig::new("slow-bind").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), None, &client_id())
        .await
        .expect("turn_start");
    // Hold the database write lock across the bind (the CLI names the session a second in).
    let mut held = rig.repo.pool().begin().await.expect("tx");
    sqlx::query("UPDATE worker_sessions SET updated_at_ms = updated_at_ms WHERE id = ?1")
        .bind(&rig.worker_session_id)
        .execute(&mut *held)
        .await
        .expect("take the write lock");
    tokio::time::sleep(Duration::from_millis(6_500)).await;
    held.rollback().await.expect("release");
    let completed = completed_turn(&until_completed(&mut rx).await);
    assert_eq!(completed["status"], "completed");
    assert_eq!(rig.agent_session_id().await, None, "the bind was cut off");

    std::fs::write(rig.bin("scenario"), "exit").expect("scenario");
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("again"), None, &client_id())
        .await
        .expect("turn_start");
    until_completed(&mut rx).await;
    let argv = rig.read_bin("argv").expect("argv");
    assert!(argv.lines().any(|a| a == "--resume"), "resumed from memory");
    assert_eq!(
        rig.agent_session_id().await.as_deref(),
        Some(rig.thread.as_str()),
        "the next turn retried the bind"
    );
}

/// #1791 PR4 review: a shutdown or a deletion seal that lands while the pre-spawn `--version`
/// check runs is re-checked right before the mint, so nothing is minted and nothing spawned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shutdown_or_seal_during_the_version_check_mints_nothing() {
    for seal in [false, true] {
        let rig = Rig::new("hold-version").await;
        let session = std::sync::Arc::clone(rig.session());
        let thread = rig.thread.clone();
        let text = rig.text("hello");
        let turn =
            tokio::spawn(
                async move { session.turn_start(&thread, text, None, &client_id()).await },
            );
        wait_for_file(&rig.bin("version-entered")).await;
        let shutdown = if seal {
            rig.daemon.seal_turn_thread_for_deletion(&rig.thread);
            None
        } else {
            let session = std::sync::Arc::clone(rig.session());
            let shutdown = tokio::spawn(async move { session.shutdown().await });
            // `shutdown` sets its flag before it waits for the issuance lock `turn_start` holds.
            tokio::time::sleep(Duration::from_millis(200)).await;
            Some(shutdown)
        };
        std::fs::write(rig.bin("release-version"), "").expect("release");
        let error = turn.await.expect("turn task").expect_err("refused");
        let expected = if seal { "sealed" } else { "shutting down" };
        assert!(error.to_string().contains(expected), "seal={seal}: {error}");
        if let Some(shutdown) = shutdown {
            shutdown.await.expect("shutdown task").expect("shutdown");
        }
        assert_eq!(
            rig.mcp_token_hash().await,
            None,
            "seal={seal}: nothing minted"
        );
        assert!(
            rig.read_bin("spawns").is_none(),
            "seal={seal}: nothing spawned"
        );
    }
}
