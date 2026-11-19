//! Real historical input and HTTP Close against one reconnectable protocol peer.
use super::*;
use crate::shared_codex_appserver::transport_recovery_tests as transport;

async fn historical(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    peer: &transport::Fixture,
    name: &str,
) -> (String, String, String) {
    peer.answers.lock().unwrap().insert(
        "thread/start".into(),
        json!({"result":{"thread":{"id":name}}}),
    );
    let area = post(
        app,
        cookie,
        "/api/areas",
        name,
        json!({"name":name,"color":"#123456"}),
    )
    .await;
    let created = crate::test_support::historical_track(state,
        json!({"area_id":area["id"],"planner_provider":"codex","title":"","theme":{"fg":[255,255,255],"bg":[0,0,0]}}),
        Some(name)).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker) =
        sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    (track, card, worker)
}

pub(super) async fn joined_cleanup(state: &AppState, worker: &str) {
    state.dispatcher.sweep_track_close_cleanups().await;
    let context = state.mcp_context();
    let _guard = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(guard) = context.tool_calls.fence(worker) {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the actual cleanup pass must return");
}

async fn hibernate(state: &AppState, worker: &str) {
    let handle = state.harness.get(worker).unwrap();
    assert!(handle.hibernate_if_idle().await.unwrap());
    assert!(state.harness.retire_if_same(worker, &handle));
}

#[tokio::test]
async fn close_p1_disconnected_stop_does_not_recover_unrelated_history() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let peer = transport::peer_for(
        state.shared_codex_appserver.clone(),
        fixture.repo.clone(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let (track, _, worker) = historical(&fixture, &state, &app, &cookie, &peer, "stop-A").await;
    hibernate(&state, &worker).await;
    let (_, _, other) = historical(&fixture, &state, &app, &cookie, &peer, "history-B").await;
    hibernate(&state, &other).await;
    peer.keep_peer_process_alive().await;
    peer.disconnect().await;
    peer.requests.lock().unwrap().clear();
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    joined_cleanup(&state, &worker).await;
    let requests = peer.requests.lock().unwrap().clone();
    let left = pending(&fixture, &worker).await;
    fixture.shutdown_process(&state).await;
    println!(
        "OBSERVATION {}",
        json!({"pending":left,"requests":requests})
    );
    assert!(
        !requests.iter().any(|r| matches!(
            r["method"].as_str(),
            Some("initialize" | "thread/resume" | "turn/start")
        )),
        "Close Stop must not reconnect or recover unrelated history"
    );
    assert_eq!(left, 1, "a disconnected live source leaves Stop pending");
}

#[tokio::test]
async fn close_p1_cold_historical_known_idle_or_not_loaded_can_retire() {
    for status in ["idle", "notLoaded"] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let peer = transport::peer_for(
            state.shared_codex_appserver.clone(),
            fixture.repo.clone(),
            tempfile::tempdir().unwrap(),
        )
        .await;
        let (track, _, worker) = historical(&fixture, &state, &app, &cookie, &peer, "idle-A").await;
        hibernate(&state, &worker).await;
        peer.answers.lock().unwrap().insert(
            "thread/read".into(),
            json!({"result":{"thread":{"id":"idle-A","status":{"type":status},"turns":[]}}}),
        );
        assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
        joined_cleanup(&state, &worker).await;
        assert_eq!(pending(&fixture, &worker).await, 0, "{status}");
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn close_p1_historical_unknown_first_start_survives_owner_restart() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let peer = transport::peer_for(
        state.shared_codex_appserver.clone(),
        fixture.repo.clone(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let (track, card, worker) =
        historical(&fixture, &state, &app, &cookie, &peer, "unknown-A").await;
    peer.answers
        .lock()
        .unwrap()
        .insert("turn/start".into(), json!({"result":{"turn":{}}}));
    peer.answers.lock().unwrap().insert("thread/read".into(),
        json!({"result":{"thread":{"id":"unknown-A","status":{"type":"active","activeFlags":[]},"turns":[
            {"id":"unrelated-old-turn","status":"completed"}]}}}));
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "unknown-first",
        json!({"text":"The peer received this first input, but its turn ID is unknown"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let raw: Option<String> =
                sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                    .bind(&worker)
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            if let Some(raw) = raw {
                let frame: Value = serde_json::from_str(&raw).unwrap();
                if frame["phase"] == "turn_completed" {
                    assert!(frame["last_turn_id"].is_null());
                    assert!(frame.get("canonical_inputs").is_none());
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("historical Unknown must be persisted without a turn ID");
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    joined_cleanup(&state, &worker).await;
    assert_eq!(pending(&fixture, &worker).await, 1);
    peer.disconnect().await;
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    // Boot a new host using its own empty process directory. The original
    // daemon is this test's still-live listener; production's stale-socket
    // reaper must never mistake that listener for a process it should replace.
    // Connect the new native owner explicitly to that same original endpoint.
    fixture.data_dir = Some(fixture.root.path().join("replacement-host"));
    let (state, _, _) = fixture.application().await;
    assert_eq!(
        pending(&fixture, &worker).await,
        1,
        "boot must retain the original obligation before peer attachment"
    );
    // A new host owner, the same original socket peer and still-running native
    // execution. No compensating turn/started or turn/completed is emitted.
    peer.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({"result":{"thread":{"id":"unknown-A","status":{"type":"active"},"turns":[
            {"id":"unrelated-old-turn","status":"completed"}]}}}),
    );
    peer.connect_new_owner(&state.shared_codex_appserver).await;
    peer.requests.lock().unwrap().clear();
    joined_cleanup(&state, &worker).await;
    let left = pending(&fixture, &worker).await;
    let requests = peer.requests.lock().unwrap().clone();
    fixture.shutdown_process(&state).await;
    println!(
        "OBSERVATION {}",
        json!({"pending_after_restart":left,"requests":requests})
    );
    assert_eq!(
        left, 1,
        "an empty native cache cannot prove the original Unknown start stopped"
    );
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r["method"].as_str(), Some("thread/resume" | "turn/start")))
    );
}
