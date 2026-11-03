//! Regression for #2986 through the real transport and canonical run loop.
use super::*;
use crate::db::RepoOutOfDomain as _;
use crate::test_seams::{PausePoint, install_pause_for_test};

#[tokio::test]
async fn canonical_native_notifications_survive_writer_contention() {
    let fx = Fixture::new().await;
    let (client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "contended").await;
    let paused = PausePoint::default();
    install_pause_for_test(
        "canonical-native-item-before-write",
        fx.harness.inner.card_id.as_str(),
        paused.clone(),
    );
    item(&fx, &mut peer, &thread, "contended", "blocked", "first").await;
    tokio::time::timeout(Duration::from_secs(5), paused.entered.notified())
        .await
        .unwrap();
    let saturated = PausePoint::default();
    install_pause_for_test(
        "codex-notification-backpressure",
        &fx.daemon.sock_path().to_string_lossy(),
        saturated.clone(),
    );
    let db = fx.original.repo.db().clone();
    db.writer()
        .with_writer(|session| {
            Box::pin(async {
                let tx = session.begin_immediate().await.unwrap();
                // The loop now has an actual SQL writer wait, rather than merely being unscheduled.
                paused.release.notify_one();
                send(
                    &mut peer,
                    json!({"method":"item/completed","params":{
                        "threadId":thread,"turnId":"contended",
                        "item":{"id":"intervening","type":"agentMessage","text":"must survive"}
                    }}),
                )
                .await;
                for index in 0..2048 {
                    send(
                        &mut peer,
                        json!({"method":"turn/started","params":{
                            "threadId":format!("sibling-{index}"),"turn":{"id":"unrelated"}
                        }}),
                    )
                    .await;
                }
                send(
                    &mut peer,
                    json!({"method":"item/completed","params":{
                        "threadId":thread,"turnId":"contended",
                        "item":{"id":"after-burst","type":"agentMessage","text":"tail"}
                    }}),
                )
                .await;
                tokio::time::timeout(Duration::from_secs(5), saturated.entered.notified())
                    .await
                    .expect("fixture's bounded harness queue must backpressure the real forwarder");
                // The response follows every burst frame on the same socket. A blocked
                // forwarder must not prevent the independent transport reader receiving it.
                let rpc = tokio::spawn(async move {
                    client
                        .config_read(None, tokio::time::Instant::now() + Duration::from_secs(5))
                        .await
                });
                let call = request(&mut peer).await;
                assert_eq!(call["method"], "config/read");
                send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                rpc.await
                    .unwrap()
                    .expect("RPC response must cross the saturated notification handoff");
                saturated.release.notify_one();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let first = wait_item(&fx, "blocked").await;
    let intervening = wait_item(&fx, "intervening").await;
    let tail = wait_item(&fx, "after-burst").await;
    assert!(
        first < intervening && intervening < tail,
        "native content order must survive backpressure"
    );
    let rows = fx
        .original
        .repo
        .transcript_rows_of_thread(fx.harness.inner.card_id.as_str(), &thread)
        .await
        .unwrap();
    for (id, row) in [
        ("blocked", first),
        ("intervening", intervening),
        ("after-burst", tail),
    ] {
        assert_eq!(rows.iter().filter(|row| row.item_uuid.as_deref() == Some(id)
            && row.method == "item/completed").count(), 1,
            "{id} must persist exactly once");
        let presentations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM conversation_presentations WHERE card_id=?1 AND native_item_row_id=?2",
        )
        .bind(fx.harness.inner.card_id.as_str())
        .bind(row)
        .fetch_one(fx.original.repo.pool())
        .await
        .unwrap();
        assert_eq!(presentations, 1, "{id} must have exactly one presentation");
    }
    stop(&fx, &mut peer).await;
}
