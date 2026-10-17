use super::*;

#[tokio::test]
async fn production_http_cold_recovery_resumes_original_thread_for_new_input() {
    original_input_flow(true, true).await;
}

pub(super) async fn second_turn(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    track: &str,
    card: &str,
    worker: &str,
) {
    eprintln!(
        "stage: original execution completed, database reopened, original receipt/carrier verified"
    );
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
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
    let mut reads = 0;
    let mut resumes = 0;
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            eprintln!("stage: cold native request {}", call["method"]);
            let result = match call["method"].as_str().unwrap() {
                "thread/read" => {
                    reads += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
                }
                "thread/resume" => {
                    resumes += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "config/read" => json!({"config":{}}),
                "turn/start" => break call,
                method => panic!("cold recovery must not mint a new thread: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("new authenticated input must resume the original cold thread");
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
    assert!(crossing.2.canonical_inputs.pending.is_empty());
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
