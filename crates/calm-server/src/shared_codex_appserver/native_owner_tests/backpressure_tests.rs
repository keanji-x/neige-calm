//! Source replacement while the lossless handoff is full.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

#[tokio::test]
async fn source_swap_discards_a_notification_waiting_for_harness_capacity() {
    let daemon = daemon().await;
    let (_old, mut old_peer) = connect(&daemon).await;
    let mut receiver = daemon.subscribe_harness_notifications();
    let saturated = PausePoint::default();
    install_pause_for_test(
        "codex-notification-backpressure",
        &daemon.sock_path().to_string_lossy(),
        saturated.clone(),
    );
    for index in 0..17 {
        send(&mut old_peer, json!({"method":"item/completed","params":{
            "threadId":"thread","turnId":"old","item":{"id":format!("old-{index}"),"type":"agentMessage","text":"old"}
        }})).await;
    }
    tokio::time::timeout(Duration::from_secs(5), saturated.entered.notified())
        .await
        .unwrap();
    let (_new, mut new_peer) = connect(&daemon).await;
    send(&mut new_peer, json!({"method":"item/completed","params":{
        "threadId":"thread","turnId":"current","item":{"id":"fresh","type":"agentMessage","text":"fresh"}
    }})).await;
    // An unscoped diagnostic remains available from the retired source and
    // witnesses that its blocked native publication has finished the source check.
    send(
        &mut old_peer,
        json!({"method":"fixture/old-forwarder-finished","params":{}}),
    )
    .await;
    saturated.release.notify_one();
    let mut old_items = Vec::new();
    let mut fresh = false;
    let mut old_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fresh || !old_finished {
            let notification = receiver.recv().await.unwrap();
            match notification.as_ref() {
                Notification::Item { params, .. } if params["item"]["id"] == "fresh" => {
                    fresh = true
                }
                Notification::Item { params, .. } => {
                    old_items.push(params["item"]["id"].as_str().unwrap().to_owned())
                }
                Notification::Other { method, .. }
                    if method == "fixture/old-forwarder-finished" =>
                {
                    old_finished = true
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        old_items,
        (0..16)
            .map(|index| format!("old-{index}"))
            .collect::<Vec<_>>(),
        "only notifications published before source replacement may survive"
    );
}
