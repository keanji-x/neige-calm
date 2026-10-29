//! G1 exercises the public creation owner, without provider processes.
use super::*;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

fn body(b: &Boot, provider: &str, attach: bool) -> Value {
    json!({"area_id": b.area_id, "planner_provider": provider,
        "attach_folder": attach, "theme": {"fg":[255,255,255],"bg":[0,0,0]}})
}

async fn empty_identity(b: &Boot, track: &Value, provider: &str) -> (String, String, String) {
    let track_id = track["id"].as_str().unwrap();
    let cards = b.repo.cards_by_track(track_id).await.unwrap();
    assert_eq!(cards.len(), 2);
    let planner = cards
        .iter()
        .find(|c| b.state.write().verify_role(&c.id) == Some(calm_server::model::CardRole::Planner))
        .unwrap();
    let report = cards
        .iter()
        .find(|c| {
            b.state.write().verify_role(&c.id) == Some(calm_server::model::CardRole::ReportCard)
        })
        .unwrap();
    assert_eq!(planner.payload["planner_provider"], provider);
    let rows: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT id, thread_id, state, handle_state_json FROM worker_sessions WHERE card_id=?1",
    )
    .bind(planner.id.as_str())
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "exactly one carrier, no historical row");
    let (runtime, thread, state, raw) = &rows[0];
    assert_eq!(state, "idle");
    assert!(thread.is_none(), "native identity waits for first input");
    let current: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(planner.id.as_str())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(&current, runtime);
    let frame: CanonicalHarnessSnapshot =
        serde_json::from_str(raw).expect("strict canonical frame rejects historical body fields");
    assert!(frame.canonical_inputs.pending.is_empty());
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert!(frame.canonical_inputs.active.is_none());
    assert_eq!(
        b.operation_count().await,
        0,
        "empty creation starts nothing"
    );
    for table in [
        "conversation_messages",
        "conversation_turns",
        "conversation_presentations",
        "conversation_human_track_creations",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "empty creation fabricates no {table}");
    }
    let persisted = b.repo.track_get(track_id).await.unwrap().unwrap();
    assert_eq!(
        persisted.workspace.kind,
        calm_types::model::TrackWorkspaceKind::Managed
    );
    assert_eq!(
        persisted.workspace.agent_cwd(),
        b.state
            .workspace_root()
            .join(&b.area_id)
            .join(track_id)
            .to_str()
            .unwrap()
    );
    assert!(persisted.workspace.worktree.is_none());
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(claims, 0, "omitted cwd claims no folder");
    (
        planner.id.to_string(),
        report.id.to_string(),
        runtime.clone(),
    )
}

#[tokio::test]
async fn g1_codex_message_less_keyed_and_unkeyed_are_empty_canonical() {
    let b = boot_without_daemon().await;
    let request = body(&b, "codex", false);
    let (status, first) = b.post_create(Some("g1-empty"), request.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let identity = empty_identity(&b, &first, "codex").await;
    let (status, replay) = b.post_create(Some("g1-empty"), request.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(empty_identity(&b, &replay, "codex").await, identity);
    let mut changed = request.clone();
    changed["attach_folder"] = json!(true);
    let (status, conflict) = b.post_create(Some("g1-empty"), changed).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    for _ in 0..2 {
        let (status, fresh) = b.post_create(None, request.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{fresh}");
        assert_ne!(fresh["id"], first["id"]);
        empty_identity(&b, &fresh, "codex").await;
    }
    assert_eq!(b.track_count().await, 3);
    assert_eq!(b.binding_count().await, 1);
}

#[tokio::test]
async fn g1_attach_without_cwd_uses_supported_empty_canonical_bindings() {
    for provider in ["codex", "claude", "opencode", "grok"] {
        let b = boot_without_daemon().await;
        let request = body(&b, provider, true);
        let (status, first) = b.post_create(Some("g1-attach"), request.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{provider}: {first}");
        let identity = empty_identity(&b, &first, provider).await;
        let (status, replay) = b.post_create(Some("g1-attach"), request.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{replay}");
        assert_eq!(replay["id"], first["id"]);
        assert_eq!(empty_identity(&b, &replay, provider).await, identity);
        let (status, fresh) = b.post_create(None, request).await;
        assert_eq!(status, StatusCode::CREATED, "{fresh}");
        assert_ne!(fresh["id"], first["id"]);
        empty_identity(&b, &fresh, provider).await;
        assert_eq!(b.binding_count().await, 1);
    }
}

#[tokio::test]
async fn g1_codex_first_input_activates_the_original_empty_carrier() {
    let b = boot().await;
    let (status, track) = b
        .post_create(Some("g1-lazy"), body(&b, "codex", false))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let (card, _, runtime) = empty_identity(&b, &track, "codex").await;
    let (status, ack) = b
        .send_planner_input(&card, "G1 first canonical input")
        .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert!(
        b.started_turn_text("G1 first canonical input")
            .await
            .contains("G1 first canonical input")
    );
    let current = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.id, runtime);
    assert!(current.thread_id.is_some());
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn g1_historical_empty_receipts_replay_original_ids_and_format() {
    for attach in [false, true] {
        let b = boot().await;
        let original = crate::support::planner::historical_track(
            &b.state,
            body(&b, "codex", attach),
            Some("g1-old"),
        )
        .await
        .unwrap();
        let cards = b
            .repo
            .cards_by_track(original["id"].as_str().unwrap())
            .await
            .unwrap();
        let planner = cards
            .iter()
            .find(|c| {
                b.state.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::Planner)
            })
            .unwrap()
            .id
            .to_string();
        let report = cards
            .iter()
            .find(|c| {
                b.state.card_role_cache.get(&c.id) == Some(calm_server::model::CardRole::ReportCard)
            })
            .unwrap()
            .id
            .to_string();
        let runtime: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
            .bind(&planner)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
        let receipt = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT json_array(track_id,planner_card_id,report_card_id, \
                 request_fingerprint_version,create_request_sha256,first_message_sha256) \
                 FROM track_create_idempotency",
            )
            .fetch_one(b.repo.pool())
            .await
            .unwrap()
        };
        let before = receipt().await;
        let app = b.app_with_running_daemon().await;
        for _ in 0..2 {
            let (status, replay) = b
                .post_create_on(app.clone(), Some("g1-old"), body(&b, "codex", attach))
                .await;
            assert_eq!(status, StatusCode::CREATED, "{replay}");
            assert_eq!(replay["id"], original["id"]);
            assert_eq!(
                receipt().await,
                before,
                "released receipt stays byte-identical"
            );
            let rows: Vec<(String, String, String)> =
                sqlx::query_as("SELECT card_id,handle_state_json,id FROM worker_sessions")
                    .fetch_all(b.repo.pool())
                    .await
                    .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, planner);
            assert_eq!(rows[0].2, runtime);
            assert!(
                serde_json::from_str::<CanonicalHarnessSnapshot>(&rows[0].1).is_err(),
                "replay retains historical format"
            );
        }
        assert!(b.repo.card_get(&report).await.unwrap().is_some());
        assert_eq!(b.track_count().await, 1);
        b.shutdown_harnesses().await;
    }
}

#[tokio::test]
async fn g1_excluded_shapes_do_not_gain_canonical_storage() {
    let b = boot_without_daemon().await;
    for extra in [
        json!({"isolation":"none"}),
        json!({"cwd":""}),
        json!({"allow_cross_area_cwd":{"folder_id":1,"area_id":b.area_id}}),
        json!({"planner_provider":"unknown"}),
    ] {
        let mut request = body(&b, "codex", true);
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (status, error) = b.post_create(None, request).await;
        assert!(status.is_client_error(), "{extra}: {status} {error}");
    }
    assert_eq!(b.track_count().await, 0);
    let mut named = body(&b, "codex", true);
    named["template_id"] = json!("investigation");
    let (status, track) = b.post_create(None, named).await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    assert_eq!(b.count("SELECT COUNT(*) FROM worker_sessions").await, 0);
    let mut admin = body(&b, "claude", false);
    admin["admin"] = json!(true);
    let (status, error) = b.post_create(Some("g1-admin"), admin).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "bad_request");
    assert_eq!(b.track_count().await, 1);
    assert_eq!(
        b.count("SELECT COUNT(*) FROM worker_sessions").await,
        0,
        "unsupported admin binding retains its historical no-daemon path"
    );
}
