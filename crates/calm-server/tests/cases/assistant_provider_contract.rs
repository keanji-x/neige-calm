//! Production JSON compatibility and persisted Assistant identity.
use super::*;

#[tokio::test]
async fn assistant_provider_unconfigured_claude_does_not_silently_start_codex() {
    let boot = boot().await;
    let track = boot.create_track("Assistant provider admission").await;
    let (status, body) = boot
        .request(
            "POST",
            &format!("/api/tracks/{track}/conversations"),
            Some("claude-assistant"),
            Some(json!({"provider": "claude", "text": "Hello"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    let (_, conversations) = boot.list_conversations(&track).await;
    assert_eq!(conversations, json!([]));
}

#[tokio::test]
async fn assistant_provider_codex_identity_is_sticky_and_new_wire_conflicts_on_provider() {
    let b = boot().await;
    let track = b.create_track("provider binding").await;
    let uri = format!("/api/tracks/{track}/conversations");
    let input = json!({"provider":"codex","text":"hello"});
    let (status, row) = b
        .request("POST", &uri, Some("typed"), Some(input.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["provider"], "codex");
    let card = row["id"].as_str().unwrap();
    let (catalog_status, _) = b
        .request(
            "GET",
            &format!("/api/models?card_id={card}&provider=claude"),
            None,
            None,
        )
        .await;
    assert_eq!(catalog_status, StatusCode::BAD_REQUEST);
    let root: Option<String> = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(&track)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let identity: (String, String, String) = sqlx::query_as("SELECT provider,contract,mode FROM worker_sessions WHERE card_id=?1 AND state!='superseded'").bind(card).fetch_one(b.repo.pool()).await.unwrap();
    assert_eq!(
        identity,
        ("codex".into(), "assistant".into(), "resumable".into())
    );
    let current: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_ne!(root.as_deref(), Some(current.as_str()));
    let (retry, same) = b.request("POST", &uri, Some("typed"), Some(input)).await;
    assert_eq!(retry, StatusCode::CREATED, "{same}");
    assert_eq!(same["id"], card);
    for provider in ["claude", "opencode"] {
        let (status, error) = b
            .request(
                "POST",
                &uri,
                Some("typed"),
                Some(json!({"provider":provider,"text":"hello"})),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "idempotency_key_reused");
    }
    for changed in [
        json!({"provider":"codex","text":"changed"}),
        json!({"provider":"codex","text":"hello","model":"different-model"}),
        json!({"provider":"codex","text":"hello","side":{"source_card_id":card,"context":"changed snapshot"}}),
    ] {
        let (status, error) = b.request("POST", &uri, Some("typed"), Some(changed)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "idempotency_key_reused");
    }
    for value in [json!("claude"), Value::Null] {
        let (status, _) = b
            .request(
                "PATCH",
                &format!("/api/cards/{card}"),
                None,
                Some(json!({"payload":{"schemaVersion":1,"assistant_provider":value}})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        b.repo.card_get(card).await.unwrap().unwrap().payload["assistant_provider"],
        "codex"
    );
}

#[tokio::test]
async fn assistant_provider_codex_failed_carrier_send_preserves_thread_and_queue() {
    use calm_server::db::sqlite::{
        session_set_handle_state_of_any_runtime_tx, session_set_status_tx,
    };
    use calm_server::harness::{HarnessPhaseTag, QueueEntry};
    use calm_server::session_projection_repo::WorkerSessionState;
    let b = boot().await;
    let track = b.create_track("failed Assistant carrier").await;
    let (status, body) = b
        .create_conversation(&track, "failed-carrier", "initial")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let card = body["id"].as_str().unwrap().to_owned();
    let runtime = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.remove(&runtime.id).unwrap();
    handle.shutdown().await.unwrap();
    let mut snapshot = handle.snapshot().await;
    let entry = QueueEntry::user_message("retained input".into(), None, Vec::new());
    snapshot.set_pending_entries(vec![entry.clone()]);
    snapshot.phase = HarnessPhaseTag::Wedged;
    snapshot.wedged_reason = Some("system_error".into());
    let mut tx = b.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::session_bind_attribution_tx(
        &mut tx,
        &runtime.id,
        calm_server::session_projection_repo::ThreadAttribution {
            worker_session_id: runtime.id.clone(),
            provider: calm_server::session_projection_repo::AgentProvider::Codex,
            thread_id: runtime.thread_id.clone(),
            session_id: None,
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    session_set_status_tx(&mut tx, &runtime.id, WorkerSessionState::Failed)
        .await
        .unwrap();
    session_set_handle_state_of_any_runtime_tx(
        &mut tx,
        &runtime.id,
        Some(serde_json::to_value(&snapshot).unwrap()),
        calm_server::model::now_ms(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    b.state.shared_codex_appserver.fail_turn_start_for_test();
    let (status, response) = b
        .request(
            "POST",
            &format!("/api/cards/{card}/planner/input"),
            Some("recover-carrier"),
            Some(json!({"text":"continue"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["worker_session_id"], runtime.id);
    let recovered = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.thread_id, runtime.thread_id);
    let snapshot = calm_server::harness::HarnessSnapshot::from_value_strict(
        recovered.handle_state_json.unwrap(),
    );
    assert!(snapshot.pending_entries().contains(&entry));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "failed carrier recovery must not replace identity"
    );
}

#[tokio::test]
async fn assistant_provider_explicit_null_and_unknown_are_not_legacy_codex() {
    let b = boot().await;
    let track = b.create_track("invalid explicit provider").await;
    for provider in [Value::Null, json!("unknown")] {
        let (status, body) = b
            .request(
                "POST",
                &format!("/api/tracks/{track}/conversations"),
                Some("invalid-provider"),
                Some(json!({"provider":provider,"text":"hello"})),
            )
            .await;
        assert!(status.is_client_error(), "{status} {body}");
    }
    let (_, rows) = b.list_conversations(&track).await;
    assert_eq!(rows, json!([]));
}

#[tokio::test]
async fn assistant_provider_current_row_mismatch_refuses_catalog_model_and_reset() {
    let b = boot().await;
    let track = b.create_track("binding corruption").await;
    let (status, row) = b.create_conversation(&track, "mismatch", "hello").await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id=?1",
    )
    .bind(card)
    .execute(b.repo.pool())
    .await
    .unwrap();
    for (method, uri, body) in [
        ("GET", format!("/api/models?card_id={card}"), None),
        (
            "PUT",
            format!("/api/cards/{card}/planner/model"),
            Some(json!({"model":null,"reasoning_effort":null})),
        ),
        (
            "POST",
            format!("/api/cards/{card}/planner/reset"),
            Some(json!({})),
        ),
    ] {
        let (status, response) = b.request(method, &uri, None, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {response}");
    }
}

#[tokio::test]
async fn assistant_provider_capabilities_follow_backend_registration_and_availability() {
    let b = boot().await;
    let (status, rows) = b
        .request("GET", "/api/agent-providers/managed", None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let (_, registered) = b.request("GET", "/api/agent-providers", None, None).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), registered.as_array().unwrap().len());
    for (row, availability) in rows.iter().zip(registered.as_array().unwrap()) {
        assert_eq!(&row["availability"], availability);
        assert_eq!(row["capabilities"]["managed_conversation"], true);
        for capability in ["images", "manual_compact", "history_replacement"] {
            assert!(row["capabilities"][capability].is_boolean(), "{row}");
        }
    }
}

#[tokio::test]
async fn assistant_provider_catalog_get_put_follow_registered_backend_admission() {
    let b = boot().await;
    let track = b.create_track("registered catalogs").await;
    let (_, registrations) = b
        .request("GET", "/api/agent-providers/managed", None, None)
        .await;
    for registration in registrations.as_array().unwrap() {
        let provider = registration["availability"]["provider"].clone();
        let mut tx = b.repo.pool().begin().await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            calm_server::model::new_id(),
            calm_server::model::NewCard {
                track_id: track.clone().into(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"harness_profile":"assistant","assistant_provider":provider}),
            },
            CardRole::Assistant,
            false,
            &b.card_role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let (status, catalog) = b
            .request(
                "GET",
                &format!("/api/models?card_id={}", card.id),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{provider}: {catalog}");
        let (status, result) = b
            .request(
                "PUT",
                &format!("/api/cards/{}/planner/model", card.id),
                None,
                Some(json!({"model":null,"reasoning_effort":null})),
            )
            .await;
        if registration["capabilities"]["requires_ready_availability"] == false {
            assert_eq!(
                status,
                StatusCode::OK,
                "default does not need a catalog: {provider}: {result}"
            );
            assert_eq!(result["model"], Value::Null);
        } else {
            assert!(
                !status.is_success(),
                "unconfigured backend must not fall through to Codex: {provider}: {result}"
            );
            let stored = b.repo.card_get(card.id.as_str()).await.unwrap().unwrap();
            assert!(stored.payload.get("model").is_none());
        }
    }
}
