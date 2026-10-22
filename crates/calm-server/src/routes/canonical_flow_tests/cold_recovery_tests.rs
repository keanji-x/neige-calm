use super::*;
use axum::extract::FromRef;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Scenario {
    Resume,
    Append,
    RevokeGate,
    RevokeRead,
    RemoteActive,
    RemoteSystemError,
    HistoricalRefusal,
}

#[tokio::test]
async fn production_http_cold_recovery_preserves_appended_pending() {
    original_input_flow(true, Some(Scenario::Append)).await;
}

#[tokio::test]
async fn production_http_cold_recovery_rechecks_source_after_native_gate() {
    original_input_flow(true, Some(Scenario::RevokeGate)).await;
}

#[tokio::test]
async fn production_http_cold_recovery_rechecks_source_before_token_and_resume() {
    original_input_flow(true, Some(Scenario::RevokeRead)).await;
}

#[tokio::test]
async fn production_http_cold_recovery_resumes_original_thread_for_new_input() {
    original_input_flow(true, Some(Scenario::Resume)).await;
}

#[tokio::test]
async fn production_http_cold_recovery_holds_remote_active_without_new_input_crossing() {
    original_input_flow(true, Some(Scenario::RemoteActive)).await;
}

#[tokio::test]
async fn production_http_cold_recovery_holds_remote_system_error() {
    original_input_flow(true, Some(Scenario::RemoteSystemError)).await;
}

#[tokio::test]
async fn historical_recovery_entry_cannot_resume_canonical_storage() {
    original_input_flow(true, Some(Scenario::HistoricalRefusal)).await;
}

pub(super) async fn second_turn(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    identity: (&str, &str, &str),
    scenario: Scenario,
) {
    let (track, card, worker) = identity;
    eprintln!(
        "stage: original execution completed, database reopened, original receipt/carrier verified"
    );
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let gate = if scenario == Scenario::RevokeGate {
        Some(
            crate::shared_codex_appserver::native_owner_tests::hold_connected_client_gate(
                &state.shared_codex_appserver,
            )
            .await,
        )
    } else {
        None
    };
    let entered = (scenario == Scenario::RevokeGate).then(|| {
        state
            .shared_codex_appserver
            .expect_exact_lookup_waiter_for_test(card, THREAD)
    });
    if scenario == Scenario::HistoricalRefusal {
        let row = fixture
            .repo
            .session_projection_active_for_card(&card.to_owned())
            .await
            .unwrap()
            .unwrap();
        let wiring = state.planner_wiring();
        let factory = crate::harness::backend::factory::PlannerFactory::new(
            std::sync::Arc::downgrade(&state.shared_codex_appserver),
            &wiring.claude,
            &wiring.acp,
        );
        let restore = factory.resume_active_conversation(fixture.repo.as_ref(), &row, None);
        tokio::pin!(restore);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut restore => assert!(matches!(result, Err(crate::error::CalmError::Conflict(_)))),
                call = request(&mut peer) => panic!("historical entry emitted RPC for canonical storage: {}", call["method"]),
            }
        }).await.expect("historical entry must reject canonical storage before native I/O");
        assert_eq!(fixture.counts(card).await, (1, 1, 1, 1, 1));
        return;
    }
    let old_token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let input = post(
        app,
        cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"A genuinely new input after reopening"}),
    )
    .await;
    assert_eq!(input["worker_session_id"], worker);
    let accepted = fixture.carrier(track).await;
    assert_eq!(accepted.2.canonical_inputs.pending.len(), 1);
    assert_eq!(accepted.2.canonical_inputs.attempts.len(), 1);
    assert!(accepted.2.canonical_inputs.active.is_none());
    eprintln!(
        "stage: new HTTP input committed on original worker; pending1, original attempt1, no new active"
    );
    if scenario == Scenario::RevokeGate {
        // Exact recovery acquires its connected source before taking transition.
        // Observe that real lookup pending behind held Core, then revoke before
        // releasing it so admission must recheck the current source.
        tokio::time::timeout(Duration::from_secs(5), entered.unwrap())
            .await
            .expect("actual recovery reached the native owner behind held Core")
            .expect("exact lookup observer remains attached");
        post(app, cookie, "/api/auth/logout", "logout", json!({})).await;
        drop(gate);
        assert_recovery_held(fixture, state, &mut peer, track, card, worker, old_token).await;
        return;
    }
    drop(gate);
    let mut reads = 0;
    let mut resumes = 0;
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            eprintln!("stage: cold native request {}", call["method"]);
            let result = match call["method"].as_str().unwrap() {
                "thread/read" => {
                    reads += 1;
                    if scenario == Scenario::Append {
                        post(
                            app,
                            cookie,
                            &format!("/api/cards/{card}/planner/input"),
                            "tail",
                            json!({"text":"Keep this newer pending input"}),
                        )
                        .await;
                    } else if scenario == Scenario::RevokeRead {
                        post(app, cookie, "/api/auth/logout", "logout", json!({})).await;
                    }
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type": match scenario { Scenario::RemoteActive => "active", Scenario::RemoteSystemError => "systemError", _ => "notLoaded" }},"turns":[]}})
                }
                "thread/resume" => {
                    resumes += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "config/read" => json!({"config":{}}),
                "turn/start" => break Some(call),
                method => panic!("cold recovery must not mint a new thread: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
            if matches!(scenario, Scenario::RevokeRead | Scenario::RemoteActive | Scenario::RemoteSystemError) && reads == 1 {
                return None;
            }
        }
    })
    .await
    .expect("new authenticated input must resume the original cold thread");
    let Some(call) = call else {
        assert_eq!((reads, resumes), (1, 0));
        assert_recovery_held(fixture, state, &mut peer, track, card, worker, old_token).await;
        return;
    };
    assert_eq!((reads, resumes), (1, 1));
    assert_eq!(call["params"]["threadId"], THREAD);
    let body = call["params"]["input"].to_string();
    assert!(body.contains("A genuinely new input after reopening"));
    assert!(
        !body.contains(FIRST_BODY),
        "completed input must not be replayed"
    );
    let crossing = fixture.carrier(track).await;
    assert_eq!((crossing.0.as_str(), crossing.1.as_str()), (card, worker));
    assert_eq!(crossing.2.canonical_inputs.attempts.len(), 2);
    assert_eq!(
        crossing.2.canonical_inputs.pending.len(),
        usize::from(scenario == Scenario::Append)
    );
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"cold-second-turn"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":"cold-second-turn","status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    if scenario != Scenario::Append {
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":"cold-second-turn","status":"completed"}),
        )
        .await;
        fixture
            .wait_phase(track, HarnessPhaseTag::TurnCompleted)
            .await;
        assert_eq!(fixture.counts(card).await, (2, 2, 1, 2, 1));
    } else {
        assert_eq!(fixture.counts(card).await, (3, 3, 1, 2, 1));
        assert_eq!(
            fixture
                .carrier(track)
                .await
                .2
                .canonical_inputs
                .pending
                .len(),
            1
        );
    }
    // The accepted tail can still be finishing its source observation. Fence
    // the temporary handle removal against that real background wake.
    let lifecycle_locks = crate::state::RouteState::from_ref(state).planner_recovery_locks;
    let _lifecycle = crate::per_card_lock::lock_card(&lifecycle_locks, card).await;
    let handle = state
        .harness
        .remove(worker)
        .expect("recovered original harness");
    let mut stopping = tokio::spawn(async move { handle.shutdown().await });
    tokio::select! {
        done = &mut stopping => done.unwrap().unwrap(),
        call = request(&mut peer) => {
            assert_eq!(call["method"], "turn/interrupt");
            send(&mut peer, json!({"id":call["id"],"result":{}})).await;
            stopping.await.unwrap().unwrap();
        }
    }
}

async fn assert_recovery_held(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer,
    track: &str,
    card: &str,
    worker: &str,
    old_token: Option<String>,
) {
    let route = crate::state::RouteState::from_ref(state);
    let settled = crate::per_card_lock::lock_card(&route.planner_recovery_locks, card);
    tokio::pin!(settled);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _guard = &mut settled => {},
            call = request(peer) => panic!("held recovery emitted another private RPC: {}", call["method"]),
        }
    }).await.expect("the real recovery owner must release its card fence");
    let after = fixture.carrier(track).await;
    assert_eq!((after.0.as_str(), after.1.as_str()), (card, worker));
    assert_eq!(after.2.canonical_inputs.pending.len(), 1);
    assert_eq!(after.2.canonical_inputs.attempts.len(), 1);
    assert!(after.2.canonical_inputs.active.is_none());
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(token, old_token);
    assert_eq!(fixture.counts(card).await, (2, 2, 1, 1, 1));
    assert!(state.harness.get(&worker.to_owned()).is_none());
}
