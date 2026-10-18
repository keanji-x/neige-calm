//! Historical controls must consume the same current source/resource policy as
//! ordinary sends. Authentication is the real password issuer; only the external
//! provider uses the existing fake transport. Membership is factual fixture setup.
use super::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;

#[derive(Clone, Copy, Debug)]
enum Control {
    Edit,
    Delete,
    Steer,
    Stop,
}

impl Control {
    fn needs_running_turn(self) -> bool {
        matches!(self, Self::Steer | Self::Stop)
    }
}

async fn hand_area_to_alice(fixture: &Fixture, area_id: &str) {
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    crate::db::sqlite::area_membership::application_user_register(
        &mut tx,
        "historical-control-alice",
        "Alice",
    )
    .await
    .unwrap();
    // There is no public membership-management path yet. Prepare legitimate
    // registry/membership facts, without issuing a credential or changing a
    // runtime phase. The Area always retains an actual registered owner.
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at)
         VALUES (?1,'historical-control-alice','owner',0)",
    )
    .bind(area_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let removed = sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
        .bind(area_id)
        .bind(LEGACY_OWNER_USER_ID)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(removed.rows_affected(), 1);
    tx.commit().await.unwrap();
}

async fn historical_control_after_membership_removal(control: Control) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "control-area",
        json!({"name":"Historical controls","color":"#123456"}),
    )
    .await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "historical-track",
        json!({
            "area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        }),
    )
    .await;
    let (card, worker): (String, String) =
        sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let harness = state
        .harness
        .get(&worker)
        .expect("original historical harness");
    let input_uri = format!("/api/cards/{card}/planner/input");
    if control.needs_running_turn() {
        post(
            &app,
            &cookie,
            &input_uri,
            "start-running",
            json!({"text":"Run before membership is removed"}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.runtime_header().await.unwrap().phase != HarnessPhaseTag::TurnRunning {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the production historical driver starts the provider turn");
    }
    harness.pause_issuance_for_dev();
    let queued = post(
        &app,
        &cookie,
        &input_uri,
        "preserve-queue",
        json!({"text":"Keep this exact entry and revision"}),
    )
    .await;
    let entry = queued["entry_id"].as_str().expect("actual queue entry");
    let before = harness.snapshot().await.unwrap();
    assert_eq!(before.pending_entries().len(), 1);
    let original_pending = before.pending_entries();
    assert_eq!(original_pending[0].user_view().unwrap().rev, 0);
    hand_area_to_alice(&fixture, area["id"].as_str().unwrap()).await;
    let before_steers = state.shared_codex_appserver.steered_turns_for_test();
    let before_stops = state.shared_codex_appserver.interrupted_turns_for_test();
    let before_events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let entry_uri = format!("{input_uri}/{entry}");
    let (method, uri, body) = match control {
        Control::Edit => (
            "PATCH",
            entry_uri,
            json!({"text":"Must not replace","if_entry_rev":0}),
        ),
        Control::Delete => ("DELETE", entry_uri, json!({"if_entry_rev":0})),
        Control::Steer => (
            "POST",
            format!("{entry_uri}/steer"),
            json!({"if_entry_rev":0}),
        ),
        Control::Stop => (
            "POST",
            format!("/api/cards/{card}/planner/interrupt"),
            json!({}),
        ),
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let response_body: Value = serde_json::from_slice(&bytes).unwrap();
    let after = harness.snapshot().await.unwrap();
    let after_steers = state.shared_codex_appserver.steered_turns_for_test();
    let after_stops = state.shared_codex_appserver.interrupted_turns_for_test();
    let after_events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    // Always let the real owner clean up before the decisive assertion, including
    // the expected red run, so an unauthorized control cannot leak a task.
    state
        .harness
        .remove(&worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{control:?} must recheck current Area membership; body={response_body}"
    );
    assert_eq!(after.pending_entries(), original_pending);
    assert_eq!(after.phase, before.phase);
    assert_eq!(after.last_thread_id, before.last_thread_id);
    assert_eq!(after.last_turn_id, before.last_turn_id);
    assert_eq!(after_steers, before_steers);
    assert_eq!(after_stops, before_stops);
    assert_eq!(after_events, before_events);
}

#[tokio::test]
async fn historical_edit_refuses_removed_area_member() {
    historical_control_after_membership_removal(Control::Edit).await;
}

#[tokio::test]
async fn historical_delete_refuses_removed_area_member() {
    historical_control_after_membership_removal(Control::Delete).await;
}

#[tokio::test]
async fn historical_steer_refuses_removed_area_member() {
    historical_control_after_membership_removal(Control::Steer).await;
}

#[tokio::test]
async fn historical_stop_refuses_removed_area_member() {
    historical_control_after_membership_removal(Control::Stop).await;
}
