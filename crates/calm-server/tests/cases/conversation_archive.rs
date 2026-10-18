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
        let mut missing_archive = row.clone();
        missing_archive
            .as_object_mut()
            .unwrap()
            .remove("archivedAt");
        assert!(
            serde_json::from_value::<calm_server::model::TrackConversationSummary>(missing_archive)
                .is_err(),
            "archive metadata is required, including for dormant summaries"
        );
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
    assert_eq!(retried["initialInput"], row["initialInput"]);
    let (status, observed) = b
        .request(
            "GET",
            &format!("/api/tracks/{track}/conversations/{card}/initial-input"),
            Some("preserved"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(observed["receipt"], row["initialInput"]["receipt"]);
    assert_eq!(b.request("GET", &history_path, None, None).await.1, history);
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

// The HTTP fence is gone, but a dequeued command still belongs to the run loop.
async fn cancelled_dequeued_command_cannot_cross_archive(action: &str) {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    use std::time::Duration;
    let b = boot().await;
    let track = b.create_track("Cancelled command archive").await;
    let (status, row) = b
        .create_conversation(&track, action, "original prompt")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    finish_turn(&b, card).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card.to_string())
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.get(&runtime.id).unwrap();
    let before = handle.snapshot().await;
    let history_path = format!("/api/cards/{card}/harness/items");
    let history = b.request("GET", &history_path, None, None).await.1;
    let dequeued = PausePoint::default();
    let completed = PausePoint::default();
    install_pause_for_test("conversation-command-dequeued", card, dequeued.clone());
    install_pause_for_test("conversation-command-completed", card, completed.clone());
    let (path, body) = if action == "compact" {
        (format!("/api/cards/{card}/planner/compact"), Body::empty())
    } else {
        (format!("/api/cards/{card}/planner/input"), Body::from(json!({"text":"replacement", "replaces_turn":if action == "replace" { before.last_turn_id.clone() } else { None }}).to_string()))
    };
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", "cancelled-command")
        .body(body)
        .unwrap();
    let app = b.app.clone();
    let http = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(10), dequeued.entered.notified())
        .await
        .expect("real HTTP command dequeued");
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    let archived = archive(&b, &track, card, true).await;
    dequeued.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), completed.entered.notified())
        .await
        .expect("command settled");
    completed.release.notify_one();
    assert_eq!(archived.0, StatusCode::NO_CONTENT, "{archived:?}");
    assert!(
        b.state
            .shared_codex_appserver
            .compacted_threads_for_test()
            .is_empty(),
        "no provider compaction after archive"
    );
    assert_eq!(
        handle.snapshot().await.pending_entries().len(),
        0,
        "no replacement accepted"
    );
    assert_eq!(
        b.request("GET", &history_path, None, None).await.1,
        history,
        "history unchanged"
    );
    let bindings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planner_input_idempotency WHERE card_id=? AND idempotency_key='cancelled-command'").bind(card).fetch_one(b.repo.pool()).await.unwrap();
    assert_eq!(
        bindings, 0,
        "no cancelled command receipt bound after archive"
    );
    assert_eq!(handle.snapshot().await.last_turn_id, before.last_turn_id);
}

#[tokio::test]
async fn cancelled_dequeued_compact_cannot_cross_archive() {
    cancelled_dequeued_command_cannot_cross_archive("compact").await;
}

#[tokio::test]
async fn cancelled_dequeued_replace_cannot_cross_archive() {
    cancelled_dequeued_command_cannot_cross_archive("replace").await;
}

#[tokio::test]
async fn cancelled_dequeued_durable_send_cannot_cross_archive() {
    cancelled_dequeued_command_cannot_cross_archive("durable").await;
}

fn as_principal(b: &Boot, user_id: &str, role: &str) -> BootRequest {
    BootRequest(
        routes::router()
            .layer(Extension(Principal {
                user_id: user_id.into(),
                display_name: user_id.into(),
                role: role.into(),
                session_id: "archive-access".into(),
            }))
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .with_state(b.state.clone()),
    )
}

struct BootRequest(axum::Router);
impl BootRequest {
    async fn archive(&self, track: &str, card: &str, archived: bool) -> StatusCode {
        self.0
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/tracks/{track}/conversations/{card}/archive"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"archived":archived}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
}

#[tokio::test]
async fn archive_checks_actual_area_membership_and_system_authority() {
    use calm_server::db::sqlite::area_membership::application_user_register;
    let b = boot().await;
    let track = b.create_track("Archive authority").await;
    let card = assistant(&b, &track, "codex").await;
    let mut tx = b.repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let alice = as_principal(&b, "alice", "member");
    // Even deployment owners need UserArea membership; Actor::User supplies no identity grant.
    assert_eq!(
        alice.archive(&track, &card, true).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        as_principal(&b, "alice", "owner")
            .archive(&track, &card, true)
            .await,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO area_members VALUES(?1,'alice','member',0)")
        .bind(&b.area_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        alice.archive(&track, &card, true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        alice.archive(&track, &card, false).await,
        StatusCode::NO_CONTENT
    );
    let mut tx = b.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::area_create_system_tx(&mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // System Area is the shared deployment policy, independent of membership.
    sqlx::query("UPDATE tracks SET area_id=(SELECT id FROM areas WHERE kind='system') WHERE id=?")
        .bind(&track)
        .execute(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        alice.archive(&track, &card, true).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        as_principal(&b, LEGACY_OWNER_USER_ID, "owner")
            .archive(&track, &card, true)
            .await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn archive_and_restore_revalidate_revocation_at_commit() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let b = boot().await;
    let track = b.create_track("Revoke archive access").await;
    let card = assistant(&b, &track, "codex").await;
    for archived in [true, false] {
        if !archived {
            assert_eq!(
                archive(&b, &track, &card, true).await.0,
                StatusCode::NO_CONTENT
            );
        }
        let hook = PausePoint::default();
        install_pause_for_test("conversation-archive-before-commit", &card, hook.clone());
        let app = as_principal(&b, LEGACY_OWNER_USER_ID, "owner");
        let t = track.clone();
        let c = card.clone();
        let request = tokio::spawn(async move { app.archive(&t, &c, archived).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), hook.entered.notified())
            .await
            .unwrap();
        sqlx::query("DELETE FROM area_members WHERE area_id=? AND user_id=?")
            .bind(&b.area_id)
            .bind(LEGACY_OWNER_USER_ID)
            .execute(b.repo.pool())
            .await
            .unwrap();
        hook.release.notify_one();
        assert_eq!(request.await.unwrap(), StatusCode::FORBIDDEN);
        assert_eq!(
            b.state
                .repo
                .conversation_archived_at(&card)
                .await
                .unwrap()
                .is_some(),
            !archived
        );
        sqlx::query("INSERT INTO area_members VALUES(?1,?2,'owner',0)")
            .bind(&b.area_id)
            .bind(LEGACY_OWNER_USER_ID)
            .execute(b.repo.pool())
            .await
            .unwrap();
    }
}

#[path = "conversation_archive_persistence.rs"]
mod persistence;

#[tokio::test]
async fn repeated_archive_stays_idempotent_with_buffered_observations() {
    let b = boot().await;
    let track = b.create_track("Archived observations").await;
    let (_, row) = b
        .create_conversation(&track, "observation-archive", "first")
        .await;
    let card = row["id"].as_str().unwrap();
    finish_turn(&b, card).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card.to_string())
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.get(&runtime.id).unwrap();
    assert_eq!(
        archive(&b, &track, card, true).await.0,
        StatusCode::NO_CONTENT
    );
    handle
        .observe_for_test(
            calm_server::harness::Observation::UserMessage {
                text: "buffered observation".into(),
            },
            None,
        )
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while handle.pending_len_for_test().await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        archive(&b, &track, card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "idempotent assignment emits no duplicate event"
    );
    assert_eq!(handle.pending_len_for_test().await, 1);
}

#[path = "conversation_archive_start.rs"]
mod start;
