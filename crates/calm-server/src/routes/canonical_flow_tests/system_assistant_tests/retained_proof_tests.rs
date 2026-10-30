//! Retained BeforeSend cleanup must survive a file database/process boundary.
use super::*;

#[tokio::test]
async fn before_send_release_failure_recovers_original_system_pending_after_reboot() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&state, &app, &cookie).await;
    let accepted = summary(&state, &app, &cookie, "original", &body(1)).await;
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let original = recovery::receipts(&fixture, &card).await;
    let (worker, before) = frame(&fixture, &card).await;
    assert_eq!(before.canonical_inputs.pending.len(), 2);
    assert!(before.canonical_inputs.active.is_none());
    assert!(before.canonical_inputs.attempts.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1"
        )
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap(),
        0,
        "native activation precedes any input plan",
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("retained-claim-committed", &card, pause.clone());
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    // Stop after the real activation claim, before acquiring the native sink.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = pause.entered.notified() => break,
                call = request(&mut peer) => {
                    assert_eq!(call["method"], "config/read");
                    send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                }
            }
        }
    })
    .await
    .unwrap();
    let op: String = sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    // The only failed SQL statement releases this exact worker/operation claim.
    sqlx::query(&format!("CREATE TRIGGER reject_retained_release BEFORE UPDATE OF spawn_op_id ON worker_sessions WHEN OLD.id='{worker}' AND OLD.spawn_op_id='{op}' AND NEW.spawn_op_id IS NULL BEGIN SELECT RAISE(ABORT,'retained release fault'); END"))
        .execute(fixture.repo.pool()).await.unwrap();
    let sink = client.lock_outbound_sink_for_test().await;
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Retire the real boot producer while the request is queued at the sink.
    state.dispatcher.retire();
    drop(sink);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let terminal: bool =
                sqlx::query_scalar("SELECT phase='failed' FROM operations WHERE id=?1")
                    .bind(&op)
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            if terminal {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(
        frame(&fixture, &card).await,
        (worker.clone(), before.clone())
    );
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    let held: Option<String> =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(held.as_deref(), Some(op.as_str()));
    sqlx::query("DROP TRIGGER reject_retained_release")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    fixture.shutdown_process(&state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((client, peer, state, app, cookie, repo));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    // A production boot installs fresh legal authority; no new subject is submitted.
    let (state, _app, _cookie) = fixture.application().await;
    assert_eq!(frame(&fixture, &card).await, (worker.clone(), before));
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card: &card,
    };
    let bootstrap = recovery::cold_next(&mut peer, false).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "proof-bootstrap")
        .await;
    let body_call = next(&mut peer, false).await;
    native
        .complete(&mut peer, body_call, &body(1), "proof-body")
        .await;
    let after = phase(&fixture, &card, HarnessPhaseTag::TurnCompleted).await;
    assert!(after.canonical_inputs.pending.is_empty());
    assert_eq!(after.canonical_inputs.attempts.len(), 2);
    assert_eq!(frame(&fixture, &card).await.0, worker);
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    shutdown(&fixture, &state).await;
}
