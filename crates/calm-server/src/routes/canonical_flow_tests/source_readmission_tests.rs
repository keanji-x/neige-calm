//! A fresh real login resumes the original claim only before any crossing.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

async fn login_again(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .extension(ConnectInfo("127.0.0.1:4444".parse::<SocketAddr>().unwrap()))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username":"jian","password":"fixture-password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn wait_for_claim(
    fixture: &Fixture,
    track: &str,
    peer: &mut Peer,
) -> CanonicalHarnessSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                call = request(peer) => {
                    let result = match call["method"].as_str().unwrap() {
                        "config/read" => json!({"config":{}}),
                        "thread/start" => json!({"thread":{"id":THREAD}}),
                        method => panic!("the original marker is held before native crossing: {method}"),
                    };
                    send(peer,json!({"id":call["id"],"result":result})).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    let (_, _, frame) = fixture.carrier(track).await;
                    if frame.canonical_inputs.active.is_some() {
                        assert!(frame.canonical_inputs.pending.is_empty());
                        assert!(frame.canonical_inputs.attempts.is_empty());
                        assert_eq!(frame.header.phase,HarnessPhaseTag::IssuingTurn);
                        return frame;
                    }
                }
            }
        }
    }).await.expect("production must commit its original claim before the held marker")
}

async fn original_claim_readmission(cold: bool) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Original unattempted claim","color":"#123456"}),
    )
    .await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let create = json!({"area_id":area["id"],"title":"","planner_provider":"codex",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
    let created = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, _) = fixture.carrier(&track).await;
    // Fault only the first crossing-marker write, not the claim or its plan.
    // No test code manufactures an active execution or advances its state.
    sqlx::query(&format!(
        "CREATE TRIGGER hold_original_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
         WHEN NEW.id='{worker}' AND
           json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts') > 0
         BEGIN SELECT RAISE(ABORT,'fixture holds the original crossing marker'); END"
    ))
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    let original = wait_for_claim(&fixture, &track, &mut peer).await;
    eprintln!("stage: real claim committed; original active and zero attempts verified");
    let original_active = original.canonical_inputs.active.clone().unwrap();
    assert_eq!(original_active.submission_ids.len(), 1);
    let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;

    let (state, app, cookie, mut peer) = if cold {
        peer.close(None).await.unwrap();
        state
            .harness
            .remove(&worker)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        assert_eq!(
            fixture.carrier(&track).await.2.canonical_inputs,
            original.canonical_inputs
        );
        drop((client, peer, state, app));
        fixture.repo.pool().close().await;
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        let (state, app, cookie) = fixture.application().await;
        state
            .recover_harnesses_on_boot(BootRows::All)
            .await
            .unwrap();
        assert!(state.harness.get(&worker).is_none());
        let (_client, peer) = connect(&state.shared_codex_appserver).await;
        (state, app, cookie, peer)
    } else {
        let next_cookie = login_again(&app).await;
        assert_ne!(next_cookie, cookie);
        (state, app, next_cookie, peer)
    };
    eprintln!("stage: old issuer revoked; new real login and optional cold boot completed");
    let replay = post(&app, &cookie, "/api/tracks", "first", create).await;
    assert_eq!(replay["id"], created["id"]);
    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
    sqlx::query("DROP TRIGGER hold_original_marker")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    eprintln!(
        "stage: original immutable key replayed; release marker fault and await original native turn"
    );
    // Bound the outer observation before request()'s own five-second timeout,
    // so the missing effect still goes through the explicit fixture cleanup.
    let resumed = tokio::time::timeout(Duration::from_secs(4), async {
        let (mut reads, mut resumes) = (0, 0);
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/read" if cold => {
                    reads += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
                }
                "thread/resume" if cold => {
                    resumes += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "turn/start" => break (call, reads, resumes),
                method => panic!("re-admission must preserve the original native thread: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await;
    // Always release the real transport and loop before asserting a timeout.
    if resumed.is_err() {
        peer.close(None).await.unwrap();
        if let Some(handle) = state.harness.remove(&worker) {
            handle.shutdown().await.unwrap();
        }
    }
    let (call, reads, resumes) =
        resumed.expect("new authenticated POST must re-admit the original unattempted claim");
    assert_eq!((reads, resumes), if cold { (1, 1) } else { (0, 0) });
    assert_eq!(call["params"]["threadId"], THREAD);
    assert!(call["params"]["input"].to_string().contains(FIRST_BODY));
    let crossing = fixture.carrier(&track).await;
    assert_eq!((&crossing.0, &crossing.1), (&card, &worker));
    assert_eq!(
        crossing.2.canonical_inputs.active.as_ref(),
        Some(&original_active)
    );
    assert_eq!(crossing.2.canonical_inputs.attempts.len(), 1);
    assert!(matches!(
        crossing.2.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert_eq!(
        crossing.2.canonical_inputs.attempts[0]
            .attempt
            .submission_id,
        original_active.submission_ids[0]
    );
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
    peer.close(None).await.unwrap();
    state
        .harness
        .remove(&worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn production_http_new_login_resumes_the_original_unattempted_claim() {
    original_claim_readmission(false).await;
}

#[tokio::test]
async fn production_http_cold_login_resumes_the_original_unattempted_claim() {
    original_claim_readmission(true).await;
}
