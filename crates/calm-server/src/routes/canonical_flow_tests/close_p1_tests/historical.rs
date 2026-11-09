use super::*;

// Released historical creation still uses its existing allocation/start owner;
// Close, Reopen and both Human inputs below use the authenticated public routes.
#[tokio::test]
async fn close_p1_historical_snapshot_only_carrier_reopens_without_replay() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "historical-area",
        json!({"name":"Historical close","color":"#123456"}),
    )
    .await;
    let created = with_historical_metadata(&mut peer, crate::test_support::historical_track(&state,
        json!({"area_id":area["id"],"planner_provider":"codex","title":"","theme":{"fg":[255,255,255],"bg":[0,0,0]}}), Some("historical-close"))).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker): (String, String) =
        sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "historical-original",
        json!({"text":FIRST_BODY}),
    )
    .await;
    let first = request(&mut peer).await;
    assert_eq!(first["method"], "turn/start");
    send(
        &mut peer,
        json!({"id":first["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    wait_historical_idle(&state, &worker, false).await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    wait_historical_idle(&state, &worker, true).await;
    let handle = state.harness.get(&worker).unwrap();
    assert!(handle.hibernate_if_idle().await.unwrap());
    assert!(state.harness.retire_if_same(&worker, &handle));
    // Hibernation joins the historical serializer; its in-memory phase alone
    // can precede the final durable completion snapshot.
    let before: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    fixture
        .repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?1").bind(&worker))
        .await
        .unwrap();
    assert_eq!(close_status(&app, &cookie, track).await, StatusCode::OK);
    finish_cleanup(&fixture, &state, &worker, &mut peer).await;
    let after: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&after).unwrap(),
        serde_json::from_str::<Value>(&before).unwrap()
    );
    with_historical_metadata(
        &mut peer,
        hibernation_tests::patch_closed(&app, &cookie, track, false),
    )
    .await;
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "historical-fresh",
        json!({"text":"New historical input after Close"}),
    )
    .await;
    let next = request(&mut peer).await;
    assert_eq!(next["method"], "turn/start");
    assert_eq!(next["params"]["threadId"], THREAD);
    let input = next["params"]["input"].to_string();
    assert!(input.contains("New historical input after Close"));
    assert!(!input.contains(FIRST_BODY));
    send(
        &mut peer,
        json!({"id":next["id"],"result":{"turn":{"id":"historical-fresh-turn"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":"historical-fresh-turn","status":"inProgress"}),
    )
    .await;
    wait_historical_idle(&state, &worker, false).await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"historical-fresh-turn","status":"completed"}),
    )
    .await;
    wait_historical_idle(&state, &worker, true).await;
    hibernation_tests::shutdown(&fixture, &state, &worker, &mut peer).await;
}

async fn wait_historical_idle(state: &AppState, worker: &str, idle: bool) {
    let worker = worker.to_owned();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let phase = state
                .harness
                .get(&worker)
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .phase;
            if (idle
                && matches!(
                    phase,
                    HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
                ))
                || (!idle && phase == HarnessPhaseTag::TurnRunning)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn with_historical_metadata<T>(
    peer: &mut Peer,
    work: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(work);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut work => break result,
                call = request(peer) => {
                    let result = match call["method"].as_str().unwrap() {
                        "thread/start" => json!({"thread":{"id":THREAD}}),
                        "thread/read" | "thread/resume" => json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[],"path":null}}),
                        "config/read" => json!({"config":{}}),
                        method => panic!("historical restore cannot issue work: {method}"),
                    };
                    send(peer, json!({"id":call["id"],"result":result})).await;
                }
            }
        }
    }).await.unwrap()
}
