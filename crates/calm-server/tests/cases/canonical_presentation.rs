//! Real authenticated reads over accepted canonical facts, not a second acceptance fixture.
use super::*;
use calm_server::db::sqlite::conversation::{
    PresentationSource, allocate_presentation_tx, current_input_carrier_tx,
};
use calm_types::conversation::{MessageRevisionRef, PrivateHistoryOperationId};

#[tokio::test]
async fn removed_human_keeps_exact_identity_and_cursor_after_erasure_and_reopen() {
    let mut f = Fixture::new().await;
    let (app, cookie) = f.application().await;
    let (_, track) = post(&app, &cookie, "/api/tracks", "first", f.create_body()).await;
    let (card, _worker, snapshot) = f.carrier(track["id"].as_str().unwrap()).await;
    let reference: MessageRevisionRef =
        serde_json::from_value(snapshot["canonical_inputs"]["pending"][0]["message"].clone())
            .unwrap();
    let uri = format!("/api/cards/{card}/harness/items?contract=conversation_v1");
    let (status, before) = get(&app, &cookie, &uri).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    let operation = PrivateHistoryOperationId::parse(&calm_server::model::new_id()).unwrap();
    // Persisted presentation tombstone exercises reads without shipping a withdraw API.
    sqlx::query(
        "UPDATE conversation_presentations SET erased_by_operation_id=?1 WHERE message_id=?2",
    )
    .bind(operation.as_str())
    .bind(reference.message_id.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (status, after) = get(&app, &cookie, &uri).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["items"][0]["cursor"], before["items"][0]["cursor"]);
    assert_eq!(
        after["items"][0]["source"],
        json!({"kind":"removed","identity":{
            "kind":"human","message_id":reference.message_id.as_str()
        }})
    );
    assert!(!after.to_string().contains("Keep the original"));
    drop(app);
    f.repo.pool().close().await;
    f.repo = Arc::new(SqlxRepo::open(&f.database_url).await.unwrap());
    let (app, cookie) = f.application().await;
    assert_eq!(get(&app, &cookie, &uri).await, (StatusCode::OK, after));
}

#[tokio::test]
async fn mixed_native_page_and_events_keep_presentation_cursor_separate_from_raw_row() {
    let f = Fixture::new().await;
    let (app, cookie) = f.application().await;
    let (_, track) = post(&app, &cookie, "/api/tracks", "first", f.create_body()).await;
    let (card, worker, _) = f.carrier(track["id"].as_str().unwrap()).await;
    // Real native observation storage primitive; this fixture proves display, not END/delivery.
    let row = f
        .repo
        .fixture_transcript_insert(
            &worker,
            &card,
            track["id"].as_str().unwrap(),
            "native-thread",
            Some("native-turn"),
            Some("native-item"),
            Some("agentMessage"),
            "item/completed",
            &json!({"item":{"id":"native-item","type":"agentMessage","text":"actual reply"}})
                .to_string(),
            None,
        )
        .await
        .unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    let sequence = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: row },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(
        row, sequence,
        "independent sequences must be deliberately exercised"
    );
    let base =
        format!("/api/cards/{card}/harness/items?contract=conversation_v1&direction=desc&limit=1");
    let (status, page) = get(&app, &cookie, &base).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"][0]["source"]["kind"], "native");
    assert_eq!(page["items"][0]["source"]["item"]["id"], row);
    assert_eq!(page["items"][0]["cursor"]["position"], sequence);
    let encoded = url::form_urlencoded::byte_serialize(page["next_cursor"].to_string().as_bytes())
        .collect::<String>();
    let (status, older) = get(&app, &cookie, &format!("{base}&cursor={encoded}")).await;
    assert_eq!(status, StatusCode::OK, "{older}");
    assert_eq!(older["items"][0]["source"]["kind"], "human");
    let events: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind='harness.presentation.changed'")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        events.len(),
        1,
        "first acceptance owns its display event; raw fixture insertion does not counterfeit one"
    );
    let event: Value = serde_json::from_str(&events[0]).unwrap();
    assert_eq!(event["cursor"], older["items"][0]["cursor"]);
}

#[tokio::test]
async fn two_session_authors_and_current_membership_control_items_run_and_live() {
    use calm_server::auth::{SessionAuthority, SessionIdentity};
    let f = Fixture::new().await;
    let auth = AuthState::new(AuthConfig {
        username: Some("jian".into()),
        password: Some("password".into()),
        dev_autologin: false,
        display_name: "Jian".into(),
    });
    let owner = format!(
        "calm-session={}",
        auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin
        )
    );
    let alice = format!(
        "calm-session={}",
        auth.sessions.create(
            SessionIdentity {
                user_id: "alice".into(),
                display_name: "Alice".into(),
                role: "member".into()
            },
            SessionAuthority::PairedDevice
        )
    );
    let app = f.application_with_auth(auth).await;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    calm_server::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(&f.area)
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_, track) = post(&app, &owner, "/api/tracks", "first", f.create_body()).await;
    let (card, _, _) = f.carrier(track["id"].as_str().unwrap()).await;
    let (status, ack) = post(
        &app,
        &alice,
        &format!("/api/cards/{card}/planner/input"),
        "alice",
        json!({"text":"Keep the original accepted input"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    let paths = [
        format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
        format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
        format!("/api/cards/{card}/harness/live?contract=conversation_v1"),
    ];
    let (_, page) = get(&app, &alice, &paths[0]).await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0]["source"]["author"]["user_id"],
        LEGACY_OWNER_USER_ID
    );
    assert_eq!(items[1]["source"]["author"]["user_id"], "alice");
    assert_ne!(
        items[0]["source"]["message_id"],
        items[1]["source"]["message_id"]
    );
    for path in &paths {
        assert_eq!(get(&app, &alice, path).await.0, StatusCode::OK);
    }
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id='alice'")
        .bind(&f.area)
        .execute(f.repo.pool())
        .await
        .unwrap();
    for path in &paths {
        assert_eq!(
            get(&app, &alice, path).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            get(&app, "", path).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

#[tokio::test]
async fn failed_current_run_preserves_active_and_pending_without_promoting_the_carrier() {
    use calm_server::db::sqlite::conversation::{
        claim_initial_submission_tx, human_plan_for_pending_prefix_tx,
    };
    use calm_types::conversation::ConversationExecutionProvenance;
    use calm_types::harness::HarnessProfile;
    use calm_types::runtime::AgentProvider;
    use calm_types::worker::WorkerSessionState;
    let f = Fixture::new().await;
    let (app, cookie) = f.application().await;
    let (status, track) = post(&app, &cookie, "/api/tracks", "failed-run", f.create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let (card, worker, _) = f.carrier(track["id"].as_str().unwrap()).await;
    let (status, second) = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second-before-failure",
        json!({"text":"Still saved after the first claim"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    // Seed a retained storage state through the actual claim and session owners.
    // This read-only fixture does not claim a live source, dispatch or native END.
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let plan = human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    let claimed = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan,
        &ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: LEGACY_OWNER_USER_ID.into(),
            provider: AgentProvider::Codex,
            profile: HarnessProfile::Planner,
        },
    )
    .await
    .unwrap();
    calm_server::db::sqlite::session_set_status_tx(&mut tx, &worker, WorkerSessionState::Failed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_, _, before) = f.carrier(track["id"].as_str().unwrap()).await;
    let (status, run) = get(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["runtime_state"], "failed", "{run}");
    assert_eq!(
        run["phase"], "issuing_turn",
        "read must not rewrite the original header"
    );
    assert_eq!(run["pending_inputs"].as_array().unwrap().len(), 1);
    assert_eq!(
        run["active_input"]["submissions"][0]["logical_turn_id"],
        claimed.logical_turn_id.as_str()
    );
    assert_eq!(
        run["active_input"]["submissions"][0]["submission_id"],
        claimed.submission_id.as_str()
    );
    assert!(run["blocked_reason"].as_str().unwrap().contains("held"));
    assert_eq!(run["history_replacement_supported"], false);
    let (_, _, after) = f.carrier(track["id"].as_str().unwrap()).await;
    assert_eq!(
        after, before,
        "reading neither ends work nor authorizes a fresh claim"
    );
    let precise_reason = "The provider ended this execution with an error.";
    sqlx::query("UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.wedged_reason',?1) WHERE id=?2")
        .bind(precise_reason).bind(&worker).execute(f.repo.pool()).await.unwrap();
    let (status, failed) = get(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(failed["runtime_state"], "failed");
    assert_eq!(failed["blocked_reason"], precise_reason);
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(&worker)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "failed",
        "reading cannot restore an inactive runtime"
    );
}

#[tokio::test]
async fn historical_card_without_current_runtime_reports_explicit_null_runtime_state() {
    let f = Fixture::new().await;
    let (app, cookie) = f.application().await;
    let (status, track) = post(
        &app,
        &cookie,
        "/api/tracks",
        "historical-read",
        f.create_body(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    let card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        calm_server::model::NewCard {
            track_id: track["id"].as_str().unwrap().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        },
        calm_server::model::CardRole::Assistant,
        false,
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Normal application reconstruction seeds the role cache from the real row.
    let (app, cookie) = f.application().await;
    let (status, run) = get(
        &app,
        &cookie,
        &format!(
            "/api/cards/{}/planner/run?contract=conversation_v1",
            card.id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["input_contract"], "historical");
    assert!(
        run.get("runtime_state").is_some(),
        "required nullable wire field"
    );
    assert!(run["runtime_state"].is_null(), "no invented Idle runtime");
}

#[tokio::test]
async fn u2_imported_http_redacts_original_paths_and_preserves_erased_provenance() {
    let mut f = Fixture::new().await;
    let (app, cookie) = f.application().await;
    let (_, track) = post(&app, &cookie, "/api/tracks", "u2", f.create_body()).await;
    let (card, worker, _) = f.carrier(track["id"].as_str().unwrap()).await;
    let row = f.repo.fixture_transcript_insert(&worker,&card,track["id"].as_str().unwrap(),
        "legacy",None,None,Some("userMessage"),"item/completed",
        r#"{"item":{"type":"userMessage","content":[{"type":"localImage","path":"/private/history.png"},{"type":"inputText","text":"Imported words"}]}}"#,None).await.unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    let boundary: i64 =
        sqlx::query_scalar("SELECT MAX(sequence) FROM conversation_presentations WHERE card_id=?1")
            .bind(&card)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    let seq = calm_server::db::sqlite::conversation::import_item_tx(&mut tx, &card, row, boundary)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let uri =
        format!("/api/cards/{card}/harness/items?contract=conversation_v1&direction=desc&limit=1");
    let (status, before) = get(&app, &cookie, &uri).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["items"][0]["source"]["kind"], "imported_item");
    assert!(!before.to_string().contains("/private/history.png"));
    assert!(before["items"][0]["source"].get("author").is_none());
    let provenance = before["items"][0]["source"]["provenance"].clone();
    assert!(
        provenance["original_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256-v1:")
    );
    sqlx::query(
        "UPDATE conversation_presentations SET erased_by_operation_id=?1 WHERE sequence=?2",
    )
    .bind("c".repeat(32))
    .bind(seq)
    .execute(f.repo.pool())
    .await
    .unwrap();
    f.repo.fixture_transcript_delete(row).await.unwrap();
    let (status, after) = get(&app, &cookie, &uri).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        after["items"][0]["source"],
        json!({"kind":"removed","identity":{"kind":"imported","provenance":provenance}})
    );
    assert_eq!(after["items"][0]["cursor"], before["items"][0]["cursor"]);
    assert!(!after.to_string().contains("Imported words"));
    drop(app);
    f.repo.pool().close().await;
    f.repo = Arc::new(SqlxRepo::open(&f.database_url).await.unwrap());
    let (app, cookie) = f.application().await;
    assert_eq!(get(&app, &cookie, &uri).await, (StatusCode::OK, after));
}
