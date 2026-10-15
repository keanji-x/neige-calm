use super::*;

async fn assistant(b: &Boot, track: &str, provider: &str) -> String {
    let id = calm_server::model::new_id();
    let mut tx = b.repo.pool().begin().await.unwrap();
    card_create_with_id_tx(&mut tx, id.clone(), calm_server::model::NewCard {
        track_id: track.to_string().into(), title: Some("Saved discussion".into()),
        kind: "codex".into(), sort: None,
        payload: json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":provider}),
    }, CardRole::Assistant, false, &b.card_role_cache).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn archive(b: &Boot, track: &str, card: &str, archived: bool) -> (StatusCode, Value) {
    b.request(
        "PATCH",
        &format!("/api/tracks/{track}/conversations/{card}/archive"),
        None,
        Some(json!({"archived":archived})),
    )
    .await
}

#[tokio::test]
async fn archive_roundtrip_preserves_each_registered_provider_and_is_idempotent() {
    let b = boot().await;
    let track = b.create_track("Archives").await;
    let (status, providers) = b
        .request("GET", "/api/agent-providers/managed", None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{providers}");
    let providers = providers.as_array().unwrap();
    assert!(!providers.is_empty());
    for answer in providers {
        let provider = answer["availability"]["provider"].as_str().unwrap();
        let card = assistant(&b, &track, provider).await;
        let original = b.repo.card_get(&card).await.unwrap().unwrap();
        let (status, response) = archive(&b, &track, &card, true).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
        let (_, rows) = b.list_conversations(&track).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == card)
            .unwrap();
        let at = row["archivedAt"]
            .as_i64()
            .expect("durable archive timestamp");
        assert_eq!(row["provider"], provider);
        assert_eq!(
            archive(&b, &track, &card, true).await.0,
            StatusCode::NO_CONTENT
        );
        let (_, rows) = b.list_conversations(&track).await;
        assert_eq!(
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == card)
                .unwrap()["archivedAt"],
            at
        );
        let stored = b.repo.card_get(&card).await.unwrap().unwrap();
        assert_eq!(stored.payload, original.payload);
        assert_eq!(stored.title, original.title);
        assert_eq!(
            archive(&b, &track, &card, false).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            archive(&b, &track, &card, false).await.0,
            StatusCode::NO_CONTENT
        );
        let (_, rows) = b.list_conversations(&track).await;
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == card)
                .unwrap()["archivedAt"]
                .is_null()
        );
    }
}

#[tokio::test]
async fn archive_refuses_other_track_planner_worker_and_non_user() {
    let b = boot().await;
    let track = b.create_track("Archives").await;
    let other = b.create_track("Other").await;
    let card = assistant(&b, &track, "claude").await;
    assert_eq!(
        archive(&b, &other, &card, true).await.0,
        StatusCode::NOT_FOUND
    );
    let worker = b.mint_codex_worker_card(&track).await;
    assert_eq!(
        archive(&b, &track, &worker, true).await.0,
        StatusCode::FORBIDDEN
    );
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(&track)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        archive(&b, &track, &planner, true).await.0,
        StatusCode::FORBIDDEN
    );
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{track}/conversations/{card}/archive"))
                .header("content-type", "application/json")
                .header("x-calm-actor", "ai:codex")
                .body(Body::from(r#"{"archived":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn archived_conversation_refuses_new_input_and_session_replacement() {
    let b = boot().await;
    let track = b.create_track("Archives").await;
    let card = assistant(&b, &track, "codex").await;
    assert_eq!(
        archive(&b, &track, &card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let (status, body) = b
        .request(
            "POST",
            &format!("/api/cards/{card}/planner/input"),
            Some("archived-input"),
            Some(json!({"text":"must not queue"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("archived"),
        "{body}"
    );
    for action in ["restart", "reset"] {
        let (status, body) = b
            .request(
                "POST",
                &format!("/api/cards/{card}/planner/{action}"),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}: {body}");
    }
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id=?",
            &card
        )
        .await,
        0
    );
}

#[tokio::test]
async fn archive_refuses_a_live_reply_or_pending_first_message() {
    let b = boot().await;
    let track = b.create_track("Busy archive").await;
    let (status, row) = b.create_conversation(&track, "busy", "in progress").await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    let (status, body) = archive(&b, &track, card, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, rows) = b.list_conversations(&track).await;
    assert!(rows[0]["archivedAt"].is_null());
}

async fn finish_turn(b: &Boot, card: &str) {
    use calm_server::codex_appserver::Notification;
    use calm_server::harness::HarnessPhaseTag;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card.to_string())
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.get(&runtime.id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = handle.snapshot().await;
            if snapshot.phase == HarnessPhaseTag::TurnRunning {
                let thread = runtime.thread_id.as_ref().unwrap();
                let turn = snapshot.last_turn_id.as_ref().unwrap();
                b.state
                    .shared_codex_appserver
                    .clear_active_turn_for_test(thread);
                b.state.shared_codex_appserver.emit_notification_for_test(
                    Notification::TurnCompleted {
                        thread_id: thread.clone(),
                        turn: json!({"id":turn,"status":"completed"}),
                    },
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        loop {
            if matches!(
                handle.snapshot().await.phase,
                HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production harness finishes the simulated provider turn");
}

#[tokio::test]
async fn archive_keeps_session_history_and_replays_an_accepted_input_receipt() {
    let b = boot().await;
    let track = b.create_track("Preserve context").await;
    let (status, row) = b
        .create_conversation(&track, "preserved", "first turn")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    finish_turn(&b, card).await;
    let path = format!("/api/cards/{card}/planner/input");
    let input = json!({"text":"accepted before archive"});
    let (status, receipt) = b
        .request("POST", &path, Some("receipt"), Some(input.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    finish_turn(&b, card).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card.to_string())
        .await
        .unwrap()
        .unwrap();
    let history_path = format!("/api/cards/{card}/harness/items");
    let (_, history) = b.request("GET", &history_path, None, None).await;
    assert!(!history.as_array().unwrap().is_empty());
    assert_eq!(
        archive(&b, &track, card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let (status, replayed) = b.request("POST", &path, Some("receipt"), Some(input)).await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, receipt);
    let (status, body) = b
        .request(
            "POST",
            &path,
            Some("new-after-archive"),
            Some(json!({"text":"no new input"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        b.request(
            "POST",
            &format!("/api/cards/{card}/planner/compact"),
            None,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(b.request("GET", &history_path, None, None).await.1, history);
    // A create retry still identifies its original archived card, without minting a successor.
    let (status, retried) = b
        .create_conversation(&track, "preserved", "first turn")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{retried}");
    assert_eq!(retried["id"], card);
    assert!(retried["archivedAt"].is_number());
    assert_eq!(
        archive(&b, &track, card, false).await.0,
        StatusCode::NO_CONTENT
    );
    let restored = b
        .repo
        .session_projection_active_for_card(&card.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.id, runtime.id);
    assert_eq!(restored.thread_id, runtime.thread_id);
    let (status, result) = b
        .request(
            "POST",
            &path,
            Some("restored-input"),
            Some(json!({"text":"continue original context"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["worker_session_id"], runtime.id);
}

#[tokio::test]
async fn automatic_recovery_does_not_install_an_archived_assistant() {
    use calm_server::harness::{
        ClaimMode, HarnessRegistry, RecoveryOutcome, spawn_recovered_harness,
    };
    let b = boot().await;
    let track = b.create_track("Archive at boot").await;
    let (status, row) = b
        .create_conversation(&track, "boot-archive", "saved turn")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap().to_string();
    finish_turn(&b, &card).await;
    assert_eq!(
        archive(&b, &track, &card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let runtime = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let registry = HarnessRegistry::new();
    let daemon = b.state.shared_codex_appserver.clone();
    let outcome = spawn_recovered_harness(
        b.repo.clone(),
        EventBus::new(),
        b.repo.card_role_cache().clone(),
        b.repo.track_area_cache().clone(),
        daemon.clone(),
        daemon.thread_seals().clone(),
        &b.state.claude_planner_wiring(),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        runtime.clone(),
        ClaimMode::ReplaceIfOpen,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Skipped));
    assert!(registry.get(&runtime.id).is_none());
    assert!(
        b.state
            .repo
            .conversation_archived_at(&card)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn archive_refuses_an_unconfirmed_wedged_provider() {
    use calm_server::db::sqlite::session_set_handle_state_of_any_runtime_tx;
    use calm_server::harness::HarnessPhaseTag;
    let b = boot().await;
    let track = b.create_track("Unconfirmed provider state").await;
    let (status, row) = b.create_conversation(&track, "wedged", "saved turn").await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap().to_string();
    finish_turn(&b, &card).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.remove(&runtime.id).unwrap();
    let mut snapshot = handle.snapshot().await;
    handle.shutdown().await.unwrap();
    snapshot.phase = HarnessPhaseTag::Wedged;
    snapshot.wedged_reason = Some("provider stop did not confirm".into());
    let mut tx = b.repo.pool().begin().await.unwrap();
    session_set_handle_state_of_any_runtime_tx(
        &mut tx,
        &runtime.id,
        Some(serde_json::to_value(snapshot).unwrap()),
        calm_server::model::now_ms(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (status, body) = archive(&b, &track, &card, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        b.state
            .repo
            .conversation_archived_at(&card)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn archive_refuses_native_activity_even_when_the_snapshot_is_idle() {
    let b = boot().await;
    let track = b.create_track("Native activity").await;
    let (status, row) = b
        .create_conversation(&track, "native-busy", "saved turn")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap().to_string();
    finish_turn(&b, &card).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let thread = runtime.thread_id.as_ref().unwrap();
    b.state
        .shared_codex_appserver
        .set_active_turn_for_test(thread, "unconfirmed-native-turn");
    let (status, body) = archive(&b, &track, &card, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    b.state
        .shared_codex_appserver
        .clear_active_turn_for_test(thread);
    assert_eq!(
        archive(&b, &track, &card, true).await.0,
        StatusCode::NO_CONTENT
    );
}
