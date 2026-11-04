#![cfg(test)]
//! System-target HTTP creation consumes the same canonical owners as User targets.
use super::u8_canonical::{body, counts, create};
use super::*;

async fn system(app: &axum::Router, cookie: &str) -> Value {
    post(app, cookie, "/api/areas/system", "system", json!({})).await
}

#[tokio::test]
async fn g4_empty_creation_is_canonical_keyed_and_unkeyed() {
    for (provider, named) in canonical_planner_providers()
        .flat_map(|provider| [false, true].map(move |named| (provider.clone(), named)))
    {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = system(&app, &cookie).await;
        let mut request = body(&area, provider.wire_name());
        request["attach_folder"] = json!(true);
        if named {
            request["template_id"] = json!("small-change");
        }
        let created = post(&app, &cookie, "/api/tracks", "empty", request.clone()).await;
        let track = created["id"].as_str().unwrap();
        let (card, worker, frame) = fixture.carrier(track).await;
        assert!(frame.canonical_inputs.pending.is_empty());
        assert!(frame.canonical_inputs.active.is_none());
        assert!(frame.canonical_inputs.attempts.is_empty());
        assert!(frame.header.last_thread_id.is_none());
        assert_eq!(fixture.counts(&card).await, (0, 0, 1, 0, 0));
        assert!(!state.shared_codex_appserver.is_running());
        assert!(state.harness.get(&worker).is_none());
        let before = counts(&fixture).await;
        let replay = post(&app, &cookie, "/api/tracks", "empty", request.clone()).await;
        assert_eq!(replay["id"], created["id"]);
        let replayed = fixture.carrier(track).await;
        assert_eq!((replayed.0, replayed.1), (card, worker));
        assert_eq!(counts(&fixture).await, before);
        let (status, independent) = create(&app, &cookie, "", request.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{independent}");
        assert_ne!(independent["id"], created["id"]);
        fixture.carrier(independent["id"].as_str().unwrap()).await;
        request["title"] = json!("changed");
        assert_eq!(
            create(&app, &cookie, "empty", request).await.0,
            StatusCode::CONFLICT
        );
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn g4_first_message_preserves_human_receipt_without_provider() {
    for (provider, named) in canonical_planner_providers()
        .flat_map(|provider| [false, true].map(move |named| (provider.clone(), named)))
    {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = system(&app, &cookie).await;
        let mut request = body(&area, provider.wire_name());
        request["first_message"] = json!(FIRST_BODY);
        request["attach_folder"] = json!(true);
        if named {
            request["template_id"] = json!("small-change");
        }
        let created = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
        let track = created["id"].as_str().unwrap();
        let (card, worker, frame) = fixture.carrier(track).await;
        assert_eq!(frame.canonical_inputs.pending.len(), 1);
        assert!(frame.canonical_inputs.active.is_none());
        assert!(frame.canonical_inputs.attempts.is_empty());
        let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
        assert_eq!(
            receipt.source_user_id,
            calm_types::organization::LEGACY_OWNER_USER_ID
        );
        let crate::db::sqlite::conversation::HumanTrackCreationAck::V1 { first_message, .. } =
            &receipt.acknowledgement;
        let write_db = (fixture.repo.db()).clone();
        write_db
            .writer()
            .with_writer(|write_session| {
                Box::pin(async {
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    let message = crate::db::sqlite::conversation::message_tx(
                        &mut tx,
                        &first_message.message_id,
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        message.author,
                        calm_types::conversation::MessageAuthor::AuthenticatedHuman {
                            registered_user_id: receipt.source_user_id.clone(),
                        }
                    );
                    tx.rollback().await.unwrap();
                    let before = counts(&fixture).await;
                    let replay = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
                    assert_eq!(replay["id"], created["id"]);
                    assert_eq!(fixture.carrier(track).await.1, worker);
                    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
                    assert_eq!(counts(&fixture).await, before);
                    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
                    request["first_message"] = json!("different input");
                    assert_eq!(
                        create(&app, &cookie, "first", request).await.0,
                        StatusCode::CONFLICT
                    );
                    fixture.shutdown_process(&state).await;
                })
            })
            .await
            .expect("independent write operation")
    }
}

#[tokio::test]
async fn g4_borrowed_system_target_keeps_folder_claim_exemption() {
    for (provider, human) in canonical_planner_providers()
        .flat_map(|provider| [false, true].map(move |human| (provider.clone(), human)))
    {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = system(&app, &cookie).await;
        let path = super::u8b_canonical::repo(&fixture);
        let mut request = body(&area, provider.wire_name());
        request["cwd"] = json!(path);
        request["isolation"] = json!("none");
        request["attach_folder"] = json!(true);
        if human {
            request["first_message"] = json!(FIRST_BODY);
        }
        let created = post(&app, &cookie, "/api/tracks", "borrowed", request.clone()).await;
        let (_, worker, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
        assert_eq!(frame.canonical_inputs.pending.len(), usize::from(human));
        let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM area_folders WHERE area_id=?1")
            .bind(area["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            claims, 0,
            "System preparation must not reserve the ordinary folder namespace"
        );
        assert_eq!(created["workspace"]["path"], json!(path));
        assert_eq!(created["workspace"]["kind"], "attached");
        let replay = post(&app, &cookie, "/api/tracks", "borrowed", request.clone()).await;
        assert_eq!(replay["id"], created["id"]);
        assert_eq!(
            fixture.carrier(created["id"].as_str().unwrap()).await.1,
            worker
        );
        request["allow_cross_area_cwd"] = json!({"folder_id":1,"area_id":area["id"]});
        let before = counts(&fixture).await;
        assert_eq!(
            create(&app, &cookie, "cross", request).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(counts(&fixture).await, before);
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn g4_system_creation_refuses_non_owner_and_unregistered_owner() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let area = system(&app, &cookie).await;
    let write_db = (fixture.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                crate::db::sqlite::area_membership::application_user_register(
                    &mut tx, "alice", "Alice",
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                let token = auth.sessions.create(
                    crate::auth::SessionIdentity {
                        user_id: "alice".into(),
                        display_name: "Owner".into(),
                        role: "owner".into(),
                    },
                    crate::auth::SessionAuthority::PasswordLogin,
                );
                let alice = format!("{}={token}", crate::auth::SESSION_COOKIE);
                for human in [false, true] {
                    let mut request = body(&area, "codex");
                    if human {
                        request["first_message"] = json!(FIRST_BODY);
                    }
                    let before = counts(&fixture).await;
                    assert_eq!(
                        create(&app, &alice, "unauthorized", request).await.0,
                        StatusCode::FORBIDDEN
                    );
                    assert_eq!(counts(&fixture).await, before);
                }
                (fixture.repo.db())
                    .writer()
                    .execute(
                        sqlx::query("DELETE FROM application_users WHERE id=?1")
                            .bind(calm_types::organization::LEGACY_OWNER_USER_ID),
                    )
                    .await
                    .unwrap();
                let before = counts(&fixture).await;
                assert_eq!(
                    create(&app, &cookie, "unregistered", body(&area, "codex"))
                        .await
                        .0,
                    StatusCode::FORBIDDEN
                );
                assert_eq!(counts(&fixture).await, before);
                fixture.shutdown_process(&state).await;
            })
        })
        .await
        .expect("independent write operation")
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn g4_system_creation_rechecks_live_authority_before_commit() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    for (human, revoke_session) in [(false, true), (true, true), (false, false), (true, false)] {
        let fixture = Fixture::new().await;
        let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
        let area = system(&app, &cookie).await;
        let mut request = body(&area, "codex");
        if human {
            request["first_message"] = json!(FIRST_BODY);
        }
        let pause = PausePoint::default();
        install_pause_for_test(
            "track-canonical-allocation",
            area["id"].as_str().unwrap(),
            pause.clone(),
        );
        let creating = create(&app, &cookie, "racing", request);
        tokio::pin!(creating);
        tokio::select! {
            _ = pause.entered.notified() => {},
            result = &mut creating => panic!("creation missed its canonical allocation fence: {result:?}"),
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing allocation rendezvous"),
        }
        if revoke_session {
            auth.sessions
                .remove(cookie.split_once('=').unwrap().1)
                .unwrap();
        } else {
            (fixture.repo.db())
                .writer()
                .execute(
                    sqlx::query("DELETE FROM application_users WHERE id=?1")
                        .bind(calm_types::organization::LEGACY_OWNER_USER_ID),
                )
                .await
                .unwrap();
        }
        let before = counts(&fixture).await;
        pause.release.notify_one();
        assert_eq!(
            creating.await.0,
            if revoke_session {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(counts(&fixture).await, before);
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn g4_original_input_executes_once_and_reopens_original_record() {
    original_input_flow_for_area(false, None, crate::model::AreaKind::System).await;
}

#[tokio::test]
async fn g4_historical_system_key_replays_original_storage_and_receipt() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = system(&app, &cookie).await;
    let request = body(&area, "codex");
    let original =
        crate::test_support::historical_track(&state, request.clone(), Some("released")).await;
    let track = original["id"].as_str().unwrap();
    let before: (String, String, Option<String>) = sqlx::query_as(
        "SELECT c.id,ws.id,ws.handle_state_json FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id \
         WHERE c.track_id=?1 AND c.role='planner'",
    ).bind(track).fetch_one(fixture.repo.pool()).await.unwrap();
    assert!(
        serde_json::from_str::<CanonicalHarnessSnapshot>(before.2.as_deref().unwrap()).is_err()
    );
    let counts_before = counts(&fixture).await;
    let replay = post(&app, &cookie, "/api/tracks", "released", request.clone()).await;
    assert_eq!(replay["id"], original["id"]);
    let after: (String, String, Option<String>) = sqlx::query_as(
        "SELECT c.id,ws.id,ws.handle_state_json FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id \
         WHERE c.track_id=?1 AND c.role='planner'",
    )
    .bind(track)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!((before.0, before.1), (after.0, after.1));
    assert!(serde_json::from_str::<CanonicalHarnessSnapshot>(after.2.as_deref().unwrap()).is_err());
    assert_eq!(counts(&fixture).await, counts_before);
    let mut changed = request;
    changed["title"] = json!("changed");
    assert_eq!(
        create(&app, &cookie, "released", changed).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(counts(&fixture).await, counts_before);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn g4_canonical_storage_failure_rolls_back_without_historical_fallback() {
    for human in [false, true] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = system(&app, &cookie).await;
        let mut request = body(&area, "codex");
        if human {
            request["first_message"] = json!(FIRST_BODY);
        }
        (fixture.repo.db())
            .writer()
            .execute(sqlx::query(
                "CREATE TRIGGER reject_g4_carrier BEFORE INSERT ON worker_sessions \
            BEGIN SELECT RAISE(ABORT,'carrier unavailable'); END",
            ))
            .await
            .unwrap();
        let before = counts(&fixture).await;
        assert_eq!(
            create(&app, &cookie, "atomic", request.clone()).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(counts(&fixture).await, before);
        (fixture.repo.db())
            .writer()
            .execute(sqlx::query("DROP TRIGGER reject_g4_carrier"))
            .await
            .unwrap();
        let created = post(&app, &cookie, "/api/tracks", "atomic", request).await;
        let frame = fixture.carrier(created["id"].as_str().unwrap()).await.2;
        assert_eq!(frame.canonical_inputs.pending.len(), usize::from(human));
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn g4_attached_named_and_admin_without_cwd_use_managed_canonical_creation() {
    for provider in canonical_planner_providers() {
        for system_area in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            state.plugin.enable("gitforge").await.unwrap();
            let area = if system_area {
                system(&app, &cookie).await
            } else {
                super::u8_canonical::area(&app, &cookie, "attached named").await
            };
            let recipe = post(
                &app,
                &cookie,
                "/api/track-recipes",
                "recipe",
                json!({"title":"Frozen recipe","body":"Original method"}),
            )
            .await;
            let source = post(
                &app,
                &cookie,
                "/api/tracks",
                "source",
                body(&area, provider.wire_name()),
            )
            .await;
            for (variant, fields) in [
                ("template", json!({"template_id":"investigation"})),
                ("recipe", json!({"recipe_id":recipe["id"]})),
                (
                    "input",
                    json!({"template_id":"dev","template_input":{"merge_policy":"ask"}}),
                ),
                ("fork", json!({"fork_report_from":source["id"]})),
                ("admin", json!({"admin":true})),
            ] {
                let mut request = body(&area, provider.wire_name());
                request["attach_folder"] = json!(true);
                request
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                let before = counts(&fixture).await;
                if system_area && variant == "admin" {
                    assert_eq!(
                        create(&app, &cookie, variant, request).await.0,
                        StatusCode::FORBIDDEN
                    );
                    assert_eq!(counts(&fixture).await, before);
                    continue;
                }
                let created = post(&app, &cookie, "/api/tracks", variant, request.clone()).await;
                let track = created["id"].as_str().unwrap();
                let (card, worker, frame) = fixture.carrier(track).await;
                assert!(frame.canonical_inputs.pending.is_empty());
                assert!(frame.canonical_inputs.active.is_none());
                assert!(frame.canonical_inputs.attempts.is_empty());
                assert!(frame.header.last_thread_id.is_none());
                assert_eq!(fixture.counts(&card).await, (0, 0, 1, 0, 0));
                let starts: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind=?1")
                        .bind(crate::routes::conversations_shared::PLANNER_HARNESS_START)
                        .fetch_one(fixture.repo.pool())
                        .await
                        .unwrap();
                assert_eq!(starts, 0);
                if variant == "recipe" {
                    assert_eq!(created["recipe_id"], recipe["id"]);
                    assert_eq!(created["recipe_revision"], recipe["revision"]);
                }
                assert_eq!(created["workspace"]["kind"], "managed");
                assert!(created["workspace"]["worktree"].is_null());
                assert_eq!(created["admin_granted_at"].is_number(), variant == "admin");
                assert!(!state.shared_codex_appserver.is_running());
                assert!(state.harness.get(&worker).is_none());
                let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
                    .bind(&card)
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
                let after = counts(&fixture).await;
                let replay = post(&app, &cookie, "/api/tracks", variant, request.clone()).await;
                assert_eq!(replay, created);
                assert_eq!(fixture.carrier(track).await, (card.clone(), worker, frame));
                assert_eq!(counts(&fixture).await, after);
                let replay_payload: String =
                    sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
                        .bind(&card)
                        .fetch_one(fixture.repo.pool())
                        .await
                        .unwrap();
                assert_eq!(replay_payload, payload);
                request["title"] = json!("changed");
                assert_eq!(
                    create(&app, &cookie, variant, request).await.0,
                    StatusCode::CONFLICT
                );
                assert_eq!(counts(&fixture).await, after);
            }
            for fields in [
                json!({"template_id":"missing-template"}),
                json!({"recipe_id":"missing-recipe"}),
                json!({"fork_report_from":"missing-track"}),
                json!({"template_input":{}}),
                json!({"template_id":"investigation","recipe_id":recipe["id"]}),
            ] {
                let mut invalid = body(&area, provider.wire_name());
                invalid["attach_folder"] = json!(true);
                invalid
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                let before = counts(&fixture).await;
                assert_eq!(
                    create(&app, &cookie, "invalid", invalid).await.0,
                    StatusCode::BAD_REQUEST
                );
                assert_eq!(counts(&fixture).await, before);
            }
            let claims: i64 =
                sqlx::query_scalar("SELECT count(*) FROM area_folders WHERE area_id=?1")
                    .bind(area["id"].as_str().unwrap())
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            assert_eq!(claims, 0);
            fixture.shutdown_process(&state).await;
        }
    }
}
