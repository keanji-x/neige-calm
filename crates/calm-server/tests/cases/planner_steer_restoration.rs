//! A system-only turn can accept multiple steers before a provider system error.
use super::*;
use calm_server::harness::QueueEntry;

async fn failed_restored_pair() -> (Boot, String, String) {
    let system = QueueEntry::system(
        Observation::TaskCompleted {
            idempotency_key: "system-only-title".into(),
            result: json!({"summary":"system context"}),
            result_contract: None,
        },
        None,
    )
    .unwrap();
    let boot = boot_with_issuance(idle_snapshot(vec![system]), Issuance::Live).await;
    wait_for_turn_running(&boot).await;
    assert_eq!(title(&boot).await, Value::Null);
    let a = queue_one(&boot, "First restored A").await;
    assert_eq!(steer(&boot, &a, 0).await.0, StatusCode::OK);
    let b = queue_one(&boot, "Second restored B").await;
    assert_eq!(steer(&boot, &b, 0).await.0, StatusCode::OK);
    assert_eq!(title(&boot).await, "First restored A");
    boot.daemon
        .emit_notification_for_test(Notification::ThreadStatusChanged {
            thread_id: SEED_THREAD_ID.into(),
            status: json!({"type":"systemError"}),
        });
    end_turn(&boot, FIRST_TURN, "failed");
    // Fake notifications bypass the daemon reducer; model the provider read as settled too.
    boot.daemon.clear_active_turn_for_test(SEED_THREAD_ID);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let row = boot
            .repo
            .session_projection_by_id(&boot.worker_session_id)
            .await
            .unwrap()
            .unwrap();
        let snapshot = HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap());
        if row.status == calm_server::session_projection_repo::WorkerSessionState::Failed
            && snapshot.pending_entries().len() == 2
            && queue_changes(&boot)
                .await
                .iter()
                .filter(|(_, change)| change == "restored")
                .count()
                == 2
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "failed owner must settle both restored entries"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (boot, a, b)
}

async fn title(boot: &Boot) -> Value {
    let (status, body) = get(
        boot.app.clone(),
        format!("/api/tracks/{}", boot.planner_card.track_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == boot.planner_card.id.as_str())
        .unwrap()["title"]
        .clone()
}

async fn recover_and_drain(boot: &Boot) {
    let (status, body) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "Resume here",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], boot.worker_session_id);
    wait_until("recovered queue drain", || {
        boot.daemon.turn_start_count_for_test() >= 2
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pending(boot).await.is_empty() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !boot.daemon.resumed_threads_for_test().is_empty(),
        "real preserving recovery resumes the original thread"
    );
}

#[tokio::test]
async fn restored_failed_pair_has_no_old_projection_after_recovery_drain() {
    let (boot, _, b) = failed_restored_pair().await;
    recover_and_drain(&boot).await;
    let rows = user_rows(&transcript(&boot).await);
    assert!(
        rows.iter().all(|row| row["item_uuid"] != b),
        "the second old projection must also transfer: {rows:?}"
    );
    let text = rows
        .iter()
        .flat_map(|row| row["input_segments"].as_array().unwrap())
        .filter(|segment| segment["presentation"] == "user")
        .map(|segment| segment["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        text.matches("Second restored B").count(),
        1,
        "one delivery in transcript: {text}"
    );
    boot.registry
        .get(&boot.worker_session_id)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn restored_failed_pair_keeps_first_title_after_recovery_drain() {
    let (boot, _, _) = failed_restored_pair().await;
    recover_and_drain(&boot).await;
    assert_eq!(
        title(&boot).await,
        "First restored A",
        "no residual B projection may precede the new A+B batch"
    );
    boot.registry
        .get(&boot.worker_session_id)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn restored_failed_pair_can_be_edited_and_deleted_after_preserving_recovery() {
    let (boot, a, b) = failed_restored_pair().await;
    boot.daemon.fail_turn_start_for_test();
    let (status, body) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "Resume here",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recovered = boot.registry.get(&boot.worker_session_id).unwrap();
    recovered.pause_issuance_for_dev();
    assert!(
        boot.repo
            .transcript_projection_id(boot.planner_card.id.as_str(), &a)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        boot.repo
            .transcript_projection_id(boot.planner_card.id.as_str(), &b)
            .await
            .unwrap()
            .is_none()
    );
    let (status, body) = send_json(
        boot.app.clone(),
        "PATCH",
        format!("/api/cards/{}/planner/input/{a}", boot.planner_card.id),
        "user",
        json!({"text":"Edited A", "if_entry_rev":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(title(&boot).await, "Edited A");
    for (id, rev, next_title) in [(a, 2, "Second restored B"), (b, 1, "Resume here")] {
        let (status, body) = send_json(
            boot.app.clone(),
            "DELETE",
            format!("/api/cards/{}/planner/input/{id}", boot.planner_card.id),
            "user",
            json!({"if_entry_rev":rev}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(title(&boot).await, next_title);
    }
    let remaining = pending(&boot).await;
    let id = remaining[0]["entry_id"].as_str().unwrap();
    let (status, body) = send_json(
        boot.app.clone(),
        "DELETE",
        format!("/api/cards/{}/planner/input/{id}", boot.planner_card.id),
        "user",
        json!({"if_entry_rev":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(title(&boot).await, Value::Null);
    assert!(
        user_rows(&transcript(&boot).await)
            .iter()
            .all(|row| row["input_segments"]
                .as_array()
                .unwrap()
                .iter()
                .all(|segment| segment["presentation"] != "user")),
        "only the system-only initial projection remains"
    );
    recovered.shutdown().await.unwrap();
}
