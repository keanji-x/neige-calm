//! Each addressable input retains its identity even when a native turn batches equal words.
use super::*;

#[tokio::test]
async fn batched_equal_messages_keep_each_entry_identity_through_the_native_echo() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![
        Observation::UserMessage {
            text: "same words".into(),
        },
        Observation::UserMessage {
            text: "same words".into(),
        },
    ]);
    let references = entries
        .iter()
        .map(|entry| entry.user_ref().unwrap())
        .collect::<Vec<_>>();
    let ids = entries
        .iter()
        .map(|entry| entry.id().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_ne!(ids[0], ids[1]);
    let boot = boot(entries, false).await;
    wait_for_turn_start(&boot).await;
    let rows = wait_for_row_count(&boot, 1).await;
    let projection = &rows[0];
    assert_eq!(projection["item_uuid"], ids[0]);
    let segments = projection["input_segments"].as_array().unwrap();
    let users = segments
        .iter()
        .filter(|segment| segment["presentation"] == "user")
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["text"], users[1]["text"]);
    assert_eq!(
        users[0]["origin"]["queue_entry"],
        serde_json::to_value(&references[0]).unwrap()
    );
    assert_eq!(
        users[1]["origin"]["queue_entry"],
        serde_json::to_value(&references[1]).unwrap()
    );
    let row_id = projection["id"].clone();
    let params: Value = serde_json::from_str(projection["params"].as_str().unwrap()).unwrap();
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": "fake-turn-0001" },
            "item": { "id": "native-batch-echo", "clientId": ids[0], "type": "userMessage",
                "content": params["item"]["content"] }
        }),
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let upgraded = loop {
        let rows = get_items(&boot).await;
        if rows
            .iter()
            .any(|row| row["item_uuid"] == "native-batch-echo")
        {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "native echo did not upgrade the projection"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(upgraded.len(), 1);
    assert_eq!(upgraded[0]["id"], row_id);
    assert_eq!(upgraded[0]["input_segments"], projection["input_segments"]);
    boot.harness.shutdown().await.unwrap();
}
