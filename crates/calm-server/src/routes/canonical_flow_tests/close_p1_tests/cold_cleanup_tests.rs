use super::*;

#[tokio::test]
async fn cold_native_history_settles_the_original_close_without_resending() {
    cold_history(true, false).await;
}

#[tokio::test]
async fn cold_native_history_of_a_different_turn_keeps_the_original_close() {
    cold_history(false, false).await;
}

#[tokio::test]
async fn cold_native_history_retires_a_completed_predecessor_without_observing_its_successor() {
    cold_history(true, true).await;
}

async fn cold_history(original: bool, replaced: bool) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
    native_attempt_recovery_tests::shutdown(&state, &worker, &mut peer).await;
    assert!(
        fixture
            .carrier(&track)
            .await
            .2
            .canonical_inputs
            .active
            .is_some()
    );
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    let stop = request(&mut peer).await;
    assert_eq!(stop["method"], "turn/interrupt");
    assert_eq!(stop["params"]["threadId"], THREAD);
    assert_eq!(stop["params"]["turnId"], TURN);
    let successor = if replaced {
        Some(
            crate::harness::backend::dispatch_fixture::supersede_planner(
                fixture.repo.as_ref(),
                &card,
                &worker,
                calm_types::runtime::AgentProvider::Codex,
            )
            .await,
        )
    } else {
        None
    };
    send(&mut peer, json!({"id":stop["id"],"result":{}})).await;
    let route = crate::state::RouteState::from_ref(&state);
    let finished = crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card);
    tokio::pin!(finished);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                guard = &mut finished => { drop(guard); break; }
                call = request(&mut peer) => {
                    assert_eq!(call["params"]["threadId"], THREAD, "cleanup must never address the successor");
                    let response = match call["method"].as_str().unwrap() {
                        "thread/read" => json!({"thread":{"id":THREAD,"status":{"type":"idle"},
                            "turns":[{"id":if original {TURN} else {"foreign-turn"},"status":"completed","itemsView":"full","items":[]}]}}),
                        "turn/interrupt" | "thread/unsubscribe" => json!({}),
                        method => panic!("Close cannot replay a prompt or restore a new thread: {method}"),
                    };
                    send(&mut peer, json!({"id":call["id"],"result":response})).await;
                }
            }
        }
    }).await.unwrap();
    assert_eq!(
        pending(&fixture, &worker).await,
        if original { 0 } else { 1 },
        "the exact historical native END must discharge the original cleanup"
    );
    if let Some(successor) = successor {
        let id = successor.id.clone();
        let current = crate::db::write_in_tx_typed(fixture.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(crate::db::sqlite::session_projection_by_id_tx(tx, &id)
                    .await?
                    .unwrap())
            })
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(current).unwrap(),
            serde_json::to_value(successor).unwrap()
        );
        crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    } else {
        assert_eq!(
            fixture
                .carrier(&track)
                .await
                .2
                .canonical_inputs
                .active
                .is_none(),
            original
        );
    }
    fixture.shutdown_process(&state).await;
}
