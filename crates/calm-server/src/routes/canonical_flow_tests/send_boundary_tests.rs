//! Revoke real authority while native activation queues on the outbound sink.
use super::*;

async fn revoked_activation(membership: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Send boundary","color":"#123456"}),
    )
    .await;
    // Finish the real boot maintenance before using the pending-RPC count as
    // the activation's sink-wait barrier.
    let reload = native_request(&mut peer).await;
    assert_eq!(reload["method"], "config/mcpServer/reload");
    send(&mut peer, json!({"id":reload["id"],"result":{}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Hold the real sink directly: a blocking config/read would itself expire
    // after one second while the HTTP/SQLite preparation is still in progress.
    let sink = client.lock_outbound_sink_for_test().await;
    let create = json!({
        "area_id":area["id"],"title":"","planner_provider":"codex",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
    });
    let created = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    let track = created["id"].as_str().unwrap();
    let (card, _, before) = fixture.carrier(track).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("activation must register its RPC behind the held outbound sink");
    if membership {
        sqlx::query("DELETE FROM area_members WHERE area_id=?1")
            .bind(area["id"].as_str().unwrap())
            .execute(fixture.repo.pool())
            .await
            .unwrap();
    } else {
        auth.sessions
            .remove(cookie.split_once('=').unwrap().1)
            .unwrap();
    }
    drop(sink);
    // The operation drive fence is released only after activation returns. It
    // supplies a completion barrier for the no-next-frame branch.
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _guard = state.operation_runtime.lock_for_track_delete() => None,
            call = request(&mut peer) => {
                send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"revoked"}})).await;
                let _settled = state.operation_runtime.lock_for_track_delete().await;
                Some(call["method"].clone())
            }
        }
    }).await.unwrap();
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    let after = fixture.carrier(track).await.2;
    assert_eq!(
        after.canonical_inputs, before.canonical_inputs,
        "input stays pending and retryable"
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
    assert_eq!(unexpected, None, "revoked activation sent a native RPC");
    let marker: Option<String> =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        marker, None,
        "a BeforeSend proof must release only the unstarted claim"
    );
    if membership {
        sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) SELECT ?1,id,'owner',0 FROM application_users")
            .bind(area["id"].as_str().unwrap()).execute(fixture.repo.pool()).await.unwrap();
    }
    let fresh_cookie = source_readmission_tests::login_again(&app).await;
    let replay = post(&app, &fresh_cookie, "/api/tracks", "first", create).await;
    assert_eq!(replay["id"], created["id"]);
    let retry = request(&mut peer).await;
    assert_eq!(
        retry["method"], "thread/start",
        "fresh authority retries the original pending input"
    );
    send(&mut peer, json!({"id":retry["id"],"error":{"code":-32000,"message":"fixture stops after proving retry"}})).await;
    let _settled = state.operation_runtime.lock_for_track_delete().await;
    fixture.shutdown_process(&state).await;
    peer.close(None).await.unwrap();
}

#[tokio::test]
async fn canonical_activation_rechecks_session_after_outbound_sink_wait() {
    revoked_activation(false).await;
}

#[tokio::test]
async fn canonical_activation_rechecks_membership_after_outbound_sink_wait() {
    revoked_activation(true).await;
}
