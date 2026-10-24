//! Managed admin admission uses the authenticated production HTTP creation entry.
use super::*;
use calm_truth::db::RepoRead;
mod activation;

fn body(area: &Value, provider: &str) -> Value {
    json!({"area_id":area["id"],"planner_provider":provider,"title":"U8",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]}})
}

async fn area(app: &axum::Router, cookie: &str, name: &str) -> Value {
    post(
        app,
        cookie,
        "/api/areas",
        name,
        json!({"name":name,"color":"#123456"}),
    )
    .await
}

#[tokio::test]
async fn u8_canonical_managed_admin_commits_empty_and_human_carriers() {
    for provider in ["codex", "opencode"] {
        for human in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "managed admin").await;
            let mut body = body(&a, provider);
            body["admin"] = json!(true);
            if human {
                body["first_message"] = json!(FIRST_BODY);
            }
            let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
            let track = created["id"].as_str().unwrap();
            let (card, worker, frame) = fixture.carrier(track).await;
            assert_eq!(frame.canonical_inputs.pending.len(), usize::from(human));
            assert!(frame.canonical_inputs.active.is_none());
            assert!(frame.canonical_inputs.attempts.is_empty());
            assert_eq!(
                fixture.counts(&card).await,
                (i64::from(human), i64::from(human), 1, 0, 0)
            );
            assert!(created["admin_granted_at"].is_number());
            assert_eq!(created["workspace"]["kind"], "managed");
            assert!(created["workspace"]["worktree"].is_null());
            assert!(!std::path::Path::new(created["workspace"]["path"].as_str().unwrap()).exists());
            let claims: i64 =
                sqlx::query_scalar("SELECT count(*) FROM area_folders WHERE area_id=?1")
                    .bind(a["id"].as_str().unwrap())
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            assert_eq!(claims, 0);
            let receipt = if human {
                Some(fixture.receipt(a["id"].as_str().unwrap()).await)
            } else {
                None
            };
            assert!(!state.shared_codex_appserver.is_running());
            assert!(state.harness.get(&worker).is_none());
            if !human {
                let (status, independent) = create(&app, &cookie, "", body.clone()).await;
                assert_eq!(status, StatusCode::CREATED, "{independent}");
                assert_ne!(independent["id"], created["id"]);
                let (_, _, empty) = fixture.carrier(independent["id"].as_str().unwrap()).await;
                assert!(empty.canonical_inputs.pending.is_empty());
                assert!(independent["admin_granted_at"].is_number());
            }
            assert_eq!(
                post(&app, &cookie, "/api/tracks", "first", body).await["id"],
                created["id"]
            );
            if let Some(receipt) = receipt {
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            }
            assert_eq!(fixture.carrier(track).await, (card, worker, frame));
            fixture.shutdown_process(&state).await;
        }
    }
}

// Snapshot the real execution references and original creation binding. Activity
// timestamps are observations; none can authorize replacing these identities.
async fn retained_identity(
    fixture: &Fixture,
    area: &str,
    worker: &str,
) -> (Value, crate::db::sqlite::TrackCreateBinding, String) {
    let session = fixture
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(worker.into()))
        .await
        .unwrap()
        .unwrap();
    let references = json!({
        "id":session.id, "track":session.track_id, "provider":session.provider,
        "mode":session.mode, "contract":session.contract,
        "parent":session.parent_session_id, "requester":session.requester_session_id,
        "state":session.state, "card":session.card_id, "mcp_token_hash":session.mcp_token_hash,
        "thread":session.thread_id, "agent_session":session.agent_session_id,
        "active_turn":session.active_turn_id, "terminal_run":session.terminal_run_id,
        "spawn_operation":session.spawn_op_id, "snapshot":session.handle_state_json
    });
    let binding =
        crate::db::sqlite::track_create_idempotency_get_pool(fixture.repo.pool(), area, "first")
            .await
            .unwrap()
            .unwrap();
    let payload = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(&binding.planner_card_id)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    (references, binding, payload)
}

async fn counts(fixture: &Fixture) -> Vec<i64> {
    let mut result = Vec::new();
    for table in [
        "tracks",
        "cards",
        "worker_sessions",
        "area_folders",
        "track_create_idempotency",
        "conversation_messages",
        "conversation_message_revisions",
        "conversation_presentations",
        "conversation_human_track_creations",
    ] {
        result.push(
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap(),
        );
    }
    result
}

async fn create(app: &axum::Router, cookie: &str, key: &str, body: Value) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/tracks")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    if !key.is_empty() {
        builder = builder.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn u8_canonical_receipt_abort_rolls_back_carrier_and_admin() {
    for provider in ["codex", "opencode"] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "atomic").await;
        let mut body = body(&a, provider);
        body["admin"] = json!(true);
        body["first_message"] = json!(FIRST_BODY);
        sqlx::query(
            "CREATE TRIGGER fail_u8_receipt BEFORE INSERT ON conversation_human_track_creations
            BEGIN SELECT RAISE(ABORT,'fixture refuses U8 receipt'); END",
        )
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        let before = counts(&fixture).await;
        assert_eq!(
            create(&app, &cookie, "first", body.clone()).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(counts(&fixture).await, before);
        sqlx::query("DROP TRIGGER fail_u8_receipt")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let created = post(&app, &cookie, "/api/tracks", "first", body).await;
        assert!(created["admin_granted_at"].is_number());
        assert_eq!(
            fixture
                .carrier(created["id"].as_str().unwrap())
                .await
                .2
                .canonical_inputs
                .pending
                .len(),
            1
        );
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn u8_canonical_empty_and_pending_repoint_preserves_original_identity() {
    for provider in ["codex", "opencode"] {
        for human in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "retained").await;
            let mut body = body(&a, provider);
            body["admin"] = json!(true);
            if human {
                body["first_message"] = json!(FIRST_BODY);
            }
            let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
            let track = created["id"].as_str().unwrap();
            let original = fixture.repo.track_get(track).await.unwrap().unwrap();
            workspace_runtime::selection::materialize_workspace(
                &original.workspace,
                state.workspace_root(),
                track,
            )
            .unwrap();
            let carrier = fixture.carrier(track).await;
            let identity = retained_identity(&fixture, a["id"].as_str().unwrap(), &carrier.1).await;
            let before = counts(&fixture).await;
            let receipt = if human {
                Some(fixture.receipt(a["id"].as_str().unwrap()).await)
            } else {
                None
            };
            let target = fixture.root.path().join("target");
            crate::test_support::init_fixture_git_repo(&target);
            let response = app.clone().oneshot(Request::builder().method("PATCH").uri(format!("/api/tracks/{track}"))
                .header(header::COOKIE,&cookie).header(header::CONTENT_TYPE,"application/json")
                .body(Body::from(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}}).to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                serde_json::to_value(fixture.repo.track_get(track).await.unwrap().unwrap())
                    .unwrap(),
                serde_json::to_value(&original).unwrap()
            );
            assert_eq!(fixture.carrier(track).await, carrier);
            assert_eq!(
                retained_identity(&fixture, a["id"].as_str().unwrap(), &carrier.1).await,
                identity
            );
            assert_eq!(counts(&fixture).await, before);
            if let Some(receipt) = receipt {
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            }
            assert_eq!(
                post(&app, &cookie, "/api/tracks", "first", body).await["id"],
                created["id"]
            );
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8_canonical_admin_requires_original_rest_user_actor() {
    for provider in ["codex", "opencode"] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "authority").await;
        let mut body = body(&a, provider);
        body["admin"] = json!(true);
        body["first_message"] = json!(FIRST_BODY);
        let before = counts(&fixture).await;
        for actor in ["ai:codex", "ai:claude", "ai:planner"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/tracks")
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("Idempotency-Key", "first")
                        .header("X-Calm-Actor", actor)
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(counts(&fixture).await, before);
        }
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn u8_canonical_resources_and_mixed_admin_keep_historical_storage() {
    for admin in [false, true] {
        for shape in ["attached", "worktree", "omitted", "cross", "named-attached"] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application_with_historical_native(true).await;
            let a = area(&app, &cookie, "historical").await;
            let mut request = body(&a, "codex");
            request["admin"] = json!(admin);
            let cwd = fixture.root.path().join("repo");
            crate::test_support::init_fixture_git_repo(&cwd);
            match shape {
                "attached" | "worktree" | "named-attached" => {
                    request["cwd"] = json!(cwd);
                    request["attach_folder"] = json!(true);
                    if shape != "worktree" {
                        request["isolation"] = json!("none");
                    }
                }
                "omitted" => request["attach_folder"] = json!(true),
                "cross" => {
                    request["allow_cross_area_cwd"] = json!({"folder_id":12345,"area_id":a["id"]})
                }
                _ => unreachable!(),
            }
            if shape == "named-attached" {
                request["template_id"] = json!("small-change");
            }
            let (status, created) = create(&app, &cookie, "resource", request).await;
            let canonical: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM worker_sessions WHERE json_valid(handle_state_json) AND \
                json_extract(handle_state_json,'$.schema_version')>=2",
            )
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
            assert_eq!(
                canonical, 0,
                "fresh {shape} admin={admin} must use historical owner: {status} {created}"
            );
            if shape != "cross" {
                assert_eq!(status, StatusCode::CREATED, "{created}");
            }
            shutdown(&fixture, &state).await;
        }
    }
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn u8_canonical_managed_admin_rechecks_target_and_live_auth_before_commit() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    for provider in ["codex", "opencode"] {
        for revoke_auth in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
            let a = area(&app, &cookie, "authority race").await;
            let mut request = body(&a, provider);
            request["admin"] = json!(true);
            request["first_message"] = json!(FIRST_BODY);
            let pause = PausePoint::default();
            install_pause_for_test(
                "track-canonical-allocation",
                a["id"].as_str().unwrap(),
                pause.clone(),
            );
            let creating = create(&app, &cookie, "racing", request);
            tokio::pin!(creating);
            tokio::select! {
                _ = pause.entered.notified() => {},
                result = &mut creating => panic!("missing authoritative admission pause: {result:?}"),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing allocation rendezvous"),
            }
            if revoke_auth {
                auth.sessions
                    .remove(cookie.split_once('=').unwrap().1)
                    .unwrap();
            } else {
                sqlx::query("DELETE FROM area_members WHERE area_id=?1")
                    .bind(a["id"].as_str().unwrap())
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            let before = counts(&fixture).await;
            pause.release.notify_one();
            let result = creating.await;
            assert_eq!(
                result.0,
                if revoke_auth {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                },
                "{result:?}"
            );
            assert_eq!(counts(&fixture).await, before);
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8_canonical_managed_admin_same_key_across_instances_keeps_original_ack() {
    for provider in ["codex", "opencode"] {
        for human in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "concurrent admin").await;
            // Independent kernel sockets and runtime state; only durable storage is shared.
            let mut other_fixture = Fixture::new().await;
            other_fixture.url = fixture.url.clone();
            let (other_state, other_app, other_cookie) = other_fixture.application().await;
            let mut body = body(&a, provider);
            body["admin"] = json!(true);
            if human {
                body["first_message"] = json!(FIRST_BODY);
            }
            let before = counts(&fixture).await;
            let (left, right) = tokio::join!(
                create(&app, &cookie, "first", body.clone()),
                create(&other_app, &other_cookie, "first", body.clone())
            );
            assert_eq!(left.0, StatusCode::CREATED, "{left:?}");
            assert_eq!(right.0, StatusCode::CREATED, "{right:?}");
            assert_eq!(left.1["id"], right.1["id"]);
            let after = counts(&fixture).await;
            assert_eq!(
                after
                    .iter()
                    .zip(before)
                    .map(|(a, b)| a - b)
                    .collect::<Vec<_>>(),
                vec![
                    1,
                    2,
                    1,
                    0,
                    1,
                    i64::from(human),
                    i64::from(human),
                    i64::from(human),
                    i64::from(human)
                ]
            );
            let carrier = fixture.carrier(left.1["id"].as_str().unwrap()).await;
            if human {
                let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
                assert_eq!(
                    create(&other_app, &other_cookie, "first", body).await.1["id"],
                    left.1["id"]
                );
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            }
            assert_eq!(
                fixture.carrier(left.1["id"].as_str().unwrap()).await,
                carrier
            );
            assert_eq!(crate::area_admin::ADMIN_TOOLS, &["neige_track_add"]);
            other_fixture.shutdown_process(&other_state).await;
            fixture.shutdown_process(&state).await;
        }
    }
}
