//! Borrowed locator acceptance through authenticated application HTTP only.
use super::u8_canonical::{area, body, counts, create};
use super::*;
use calm_truth::db::RepoRead;

mod c5_admission;
mod recovery;
mod selection;
#[cfg(feature = "fixtures")]
mod u8c_fence;

const MARKER: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>committed</text></svg>";

pub(super) fn repo(fixture: &Fixture) -> std::path::PathBuf {
    let path = fixture.root.path().join("repo");
    crate::test_support::init_fixture_git_repo(&path);
    std::fs::write(path.join("marker.svg"), MARKER).unwrap();
    for args in [vec!["add", "marker.svg"], vec!["commit", "-m", "marker"]] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    path
}

pub(super) async fn http(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: &str,
) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

async fn read(app: &axum::Router, cookie: &str, track: &str, expected: &str) {
    for raw in [false, true] {
        let suffix = if raw { "-raw" } else { "" };
        let (status, bytes) = http(
            app,
            cookie,
            "GET",
            &format!("/api/tracks/{track}/workspace/readfile{suffix}?path=marker.svg"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        if raw {
            assert_eq!(bytes, expected.as_bytes());
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["text"],
                expected
            );
        }
    }
}

async fn claim(app: &axum::Router, cookie: &str, area: &Value, path: &std::path::Path) -> Value {
    post(
        app,
        cookie,
        &format!("/api/areas/{}/folders", area["id"].as_str().unwrap()),
        "claim",
        json!({"path":path}),
    )
    .await
}

async fn delete_claim(app: &axum::Router, cookie: &str, area: &Value, folder: &Value) {
    assert_eq!(
        http(
            app,
            cookie,
            "DELETE",
            &format!(
                "/api/areas/{}/folders/{}",
                area["id"].as_str().unwrap(),
                folder["id"]
            )
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

fn borrowed(a: &Value, provider: &str, path: &std::path::Path, isolation: &str) -> Value {
    let mut request = body(a, provider);
    request["cwd"] = json!(path);
    request["attach_folder"] = json!(true);
    if isolation != "default" {
        request["isolation"] = json!(isolation);
    }
    request
}

#[tokio::test]
async fn u8b_canonical_borrowed_shapes_commit_and_read_empty() {
    borrowed_shapes_commit_and_read(false).await;
}

#[tokio::test]
async fn u8b_canonical_borrowed_shapes_commit_and_read_human() {
    borrowed_shapes_commit_and_read(true).await;
}

async fn borrowed_shapes_commit_and_read(human: bool) {
    // Bound full application fixtures per test process while retaining every
    // provider/resource/input combination and the same production assertions.
    for provider in calm_types::runtime::AgentProvider::ALL {
        for shape in ["none", "default", "worktree", "same", "cross", "named"] {
            borrowed_shape_commit_and_read(provider.wire_name(), shape, human).await;
        }
    }
}

async fn borrowed_shape_commit_and_read(provider: &str, shape: &str, human: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "borrower").await;
    let path = repo(&fixture);
    let mut request = borrowed(
        &a,
        provider,
        &path,
        if shape == "none" || shape == "same" {
            "none"
        } else {
            "default"
        },
    );
    request["admin"] = json!(human);
    if human {
        request["first_message"] = json!(FIRST_BODY);
    }
    if shape == "worktree" {
        request["isolation"] = json!("worktree");
    }
    if shape == "same" {
        claim(&app, &cookie, &a, &path).await;
        request["attach_folder"] = json!(false);
    }
    if shape == "cross" {
        let source = area(&app, &cookie, "source").await;
        let folder = claim(&app, &cookie, &source, &path).await;
        request["allow_cross_area_cwd"] = json!({"area_id":source["id"],"folder_id":folder["id"]});
        request["attach_folder"] = json!(false);
    }
    if shape == "named" {
        request["template_id"] = json!("small-change");
    }
    // A worktree must read its selected commit, not the source's dirty bytes.
    std::fs::write(path.join("marker.svg"), "dirty").unwrap();
    let created = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, frame) = fixture.carrier(track).await;
    assert_eq!(frame.canonical_inputs.pending.len(), usize::from(human));
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert_eq!(
        fixture.counts(&card).await,
        (i64::from(human), i64::from(human), 1, 0, 0)
    );
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_human_track_creations")
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(receipts, i64::from(human));
    let saved = fixture.repo.track_get(track).await.unwrap().unwrap();
    assert_eq!(created["workspace"]["kind"], "attached");
    let none = shape == "none" || shape == "same";
    assert_eq!(saved.workspace.worktree.is_none(), none);
    assert_eq!(
        saved.workspace.agent_cwd(),
        if none {
            path.to_str().unwrap()
        } else {
            saved.workspace.worktree.as_deref().unwrap()
        }
    );
    read(&app, &cookie, track, if none { "dirty" } else { MARKER }).await;
    let assistant = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "assistant",
        json!({"provider":provider,"text":FIRST_BODY}),
    )
    .await;
    let assistant_id = assistant["id"].as_str().unwrap();
    let (_, assistant_frame) = super::assistant_canonical::carrier(&fixture, assistant_id).await;
    assert_eq!(assistant_frame.canonical_inputs.pending.len(), 1);
    assert_eq!(
        fixture
            .repo
            .card_get(assistant_id)
            .await
            .unwrap()
            .unwrap()
            .track_id
            .as_str(),
        track
    );
    assert_eq!(
        fixture
            .repo
            .track_get(track)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .agent_cwd(),
        saved.workspace.agent_cwd()
    );
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", request).await,
        created
    );
    assert_eq!(fixture.carrier(track).await, (card, worker, frame));
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8b_canonical_claim_changes_preserve_fixed_locator() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        for isolation in ["none", "default"] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "target").await;
            let source = area(&app, &cookie, "source").await;
            let reclaim = area(&app, &cookie, "reclaim").await;
            let path = repo(&fixture);
            let folder = claim(&app, &cookie, &source, &path).await;
            let mut request = borrowed(&a, provider, &path, isolation);
            request["first_message"] = json!(FIRST_BODY);
            request["allow_cross_area_cwd"] =
                json!({"area_id":source["id"],"folder_id":folder["id"]});
            let created = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
            let track = created["id"].as_str().unwrap();
            let before = fixture.carrier(track).await;
            let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
            delete_claim(&app, &cookie, &source, &folder).await;
            for reclaimed in [false, true] {
                if reclaimed {
                    claim(&app, &cookie, &reclaim, &path).await;
                }
                assert_eq!(
                    post(&app, &cookie, "/api/tracks", "first", request.clone()).await,
                    created
                );
                read(&app, &cookie, track, MARKER).await;
                let denied = create(&app, &cookie, "stale", request.clone()).await;
                assert!(
                    matches!(denied.0, StatusCode::BAD_REQUEST | StatusCode::CONFLICT),
                    "{denied:?}"
                );
                if reclaimed {
                    assert!(
                        denied.1["folder_id"].is_number(),
                        "structured conflict: {denied:?}"
                    );
                }
                assert_eq!(fixture.carrier(track).await, before);
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            }
            let folders = fixture
                .repo
                .area_folders_by_area(reclaim["id"].as_str().unwrap())
                .await
                .unwrap();
            request["allow_cross_area_cwd"] =
                json!({"area_id":reclaim["id"],"folder_id":folders[0].id});
            let fresh = post(&app, &cookie, "/api/tracks", "new-consent", request).await;
            assert_ne!(fresh["id"], created["id"]);
            fixture.carrier(fresh["id"].as_str().unwrap()).await;
            assert_eq!(
                http(&app, &cookie, "POST", "/api/auth/logout").await.0,
                StatusCode::OK
            );
            let uri = format!("/api/tracks/{track}/workspace/readfile?path=marker.svg");
            assert_eq!(
                http(&app, &cookie, "GET", &uri).await.0,
                StatusCode::UNAUTHORIZED
            );
            let next_cookie = super::source_readmission_tests::login_again(&app).await;
            read(&app, &next_cookie, track, MARKER).await;
            assert_eq!(fixture.carrier(track).await, before);
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8b_canonical_receipt_abort_rolls_back_resources() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "atomic").await;
        let path = repo(&fixture);
        let mut request = borrowed(&a, provider, &path, "default");
        request["admin"] = json!(true);
        request["first_message"] = json!(FIRST_BODY);
        sqlx::query(concat!(
            "CREATE TRIGGER fail_u8b BEFORE INSERT ON conversation_human_track_creations ",
            "BEGIN SELECT RAISE(ABORT,'receipt fault'); END"
        ))
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        let before = counts(&fixture).await;
        assert_eq!(
            create(&app, &cookie, "first", request.clone()).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(counts(&fixture).await, before);
        sqlx::query("DROP TRIGGER fail_u8b")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let created = post(&app, &cookie, "/api/tracks", "first", request).await;
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

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn u8b_canonical_authority_races_roll_back() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        for race in [
            "source",
            "target",
            "logout",
            "claim-delete",
            "claim-same",
            "claim-cross",
            "source-delete",
        ] {
            let fixture = Fixture::new().await;
            let (state, app, cookie, _) = fixture.application_with_auth(false).await;
            let a = area(&app, &cookie, "target").await;
            let source = area(&app, &cookie, "source").await;
            let other = area(&app, &cookie, "other").await;
            let path = repo(&fixture);
            let folder = claim(&app, &cookie, &source, &path).await;
            let mut request = borrowed(&a, provider, &path, "none");
            request["first_message"] = json!(FIRST_BODY);
            request["admin"] = json!(true);
            request["allow_cross_area_cwd"] =
                json!({"area_id":source["id"],"folder_id":folder["id"]});
            let pause = PausePoint::default();
            install_pause_for_test(
                "track-canonical-allocation",
                a["id"].as_str().unwrap(),
                pause.clone(),
            );
            let creating = create(&app, &cookie, "first", request);
            tokio::pin!(creating);
            tokio::select! {
                _ = pause.entered.notified() => {},
                result = &mut creating => panic!("missing allocation rendezvous: {result:?}"),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing allocation rendezvous"),
            }
            match race {
                "source" | "target" => {
                    let id = if race == "source" {
                        &source["id"]
                    } else {
                        &a["id"]
                    };
                    let sql = if race == "source" {
                        "UPDATE area_members SET role='member' WHERE area_id=?1"
                    } else {
                        "DELETE FROM area_members WHERE area_id=?1"
                    };
                    sqlx::query(sql)
                        .bind(id.as_str().unwrap())
                        .execute(fixture.repo.pool())
                        .await
                        .unwrap();
                }
                "source-delete" => {
                    assert_eq!(
                        http(
                            &app,
                            &cookie,
                            "DELETE",
                            &format!("/api/areas/{}", source["id"].as_str().unwrap())
                        )
                        .await
                        .0,
                        StatusCode::NO_CONTENT
                    );
                }
                "logout" => {
                    assert_eq!(
                        http(&app, &cookie, "POST", "/api/auth/logout").await.0,
                        StatusCode::OK
                    );
                }
                _ => {
                    delete_claim(&app, &cookie, &source, &folder).await;
                    if race != "claim-delete" {
                        claim(
                            &app,
                            &cookie,
                            if race == "claim-same" {
                                &source
                            } else {
                                &other
                            },
                            &path,
                        )
                        .await;
                    }
                }
            }
            let before = counts(&fixture).await;
            pause.release.notify_one();
            let result = creating.await;
            assert!(!result.0.is_success(), "{provider} {race}: {result:?}");
            assert_eq!(counts(&fixture).await, before, "{provider} {race}");
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8b_canonical_keys_replay_original_ack() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        for isolation in ["none", "default"] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "keys").await;
            let mut other_fixture = Fixture::new().await;
            other_fixture.url = fixture.url.clone();
            let (other_state, other_app, other_cookie) = other_fixture.application().await;
            let path = repo(&fixture);
            let mut request = borrowed(&a, provider, &path, isolation);
            request["first_message"] = json!(FIRST_BODY);
            let (left, right) = tokio::join!(
                create(&app, &cookie, "first", request.clone()),
                create(&other_app, &other_cookie, "first", request.clone())
            );
            assert_eq!(left.0, StatusCode::CREATED, "{left:?}");
            assert_eq!(right.0, StatusCode::CREATED, "{right:?}");
            assert_eq!(left.1, right.1);
            let track = left.1["id"].as_str().unwrap();
            let before = fixture.carrier(track).await;
            let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
            for (field, value) in [
                ("cwd", json!(path.join("other"))),
                (
                    "isolation",
                    json!(if isolation == "none" {
                        "worktree"
                    } else {
                        "none"
                    }),
                ),
                ("admin", json!(true)),
                (
                    "planner_provider",
                    json!(if provider == "claude" {
                        "grok"
                    } else {
                        "claude"
                    }),
                ),
                ("first_message", json!("changed")),
                (
                    "allow_cross_area_cwd",
                    json!({"folder_id":999,"area_id":a["id"]}),
                ),
            ] {
                let mut changed = request.clone();
                changed[field] = value;
                assert_eq!(
                    create(&app, &cookie, "first", changed).await.0,
                    StatusCode::CONFLICT,
                    "{field}"
                );
            }
            assert_eq!(fixture.carrier(track).await, before);
            assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            let (one, two) = tokio::join!(
                create(&app, &cookie, "one", request.clone()),
                create(&other_app, &other_cookie, "two", request.clone())
            );
            assert_eq!(one.0, StatusCode::CREATED, "{one:?}");
            assert_eq!(two.0, StatusCode::CREATED, "{two:?}");
            assert_ne!(one.1["id"], two.1["id"]);
            request.as_object_mut().unwrap().remove("first_message");
            let one = create(&app, &cookie, "", request.clone()).await;
            let two = create(&app, &cookie, "", request).await;
            assert_eq!(one.0, StatusCode::CREATED);
            assert_eq!(two.0, StatusCode::CREATED);
            assert_ne!(one.1["id"], two.1["id"]);
            other_fixture.shutdown_process(&other_state).await;
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8b_canonical_retained_borrowed_repoint_refused() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        for human in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let a = area(&app, &cookie, "repoint").await;
            let path = repo(&fixture);
            let mut request = borrowed(&a, provider, &path, "default");
            if human {
                request["first_message"] = json!(FIRST_BODY);
            }
            let created = post(&app, &cookie, "/api/tracks", "first", request).await;
            let track = created["id"].as_str().unwrap();
            let before = fixture.carrier(track).await;
            let saved = fixture.repo.track_get(track).await.unwrap().unwrap();
            let replacement = fixture.root.path().join("other");
            crate::test_support::init_fixture_git_repo(&replacement);
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri(format!("/api/tracks/{track}"))
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({"workspace":{"kind":"attached","path":fixture.root.path().join("other"),"attach_folder":true}})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(fixture.carrier(track).await, before);
            assert_eq!(
                fixture
                    .repo
                    .track_get(track)
                    .await
                    .unwrap()
                    .unwrap()
                    .workspace,
                saved.workspace
            );
            fixture.shutdown_process(&state).await;
        }
    }
}

#[tokio::test]
async fn u8b_canonical_invalid_resources_never_fall_back() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "invalid").await;
    let other = area(&app, &cookie, "owner").await;
    let path = repo(&fixture);
    let folder = claim(&app, &cookie, &other, &path).await;
    let bare = fixture.root.path().join("not-git");
    std::fs::create_dir_all(&bare).unwrap();
    let owned = fixture.root.path().join("workspaces/borrowed");
    std::fs::create_dir_all(&owned).unwrap();
    let link = fixture.root.path().join("physical-owned");
    std::os::unix::fs::symlink(&owned, &link).unwrap();
    for case in [
        "unconfirmed",
        "wrong-area",
        "wrong-folder",
        "omitted-cross",
        "missing",
        "non-git",
        "relative",
        "owned",
        "physical-owned",
        "actor",
    ] {
        let mut request = borrowed(&a, "codex", &path, "none");
        request["first_message"] = json!(FIRST_BODY);
        match case {
            "wrong-area" | "wrong-folder" => {
                request["allow_cross_area_cwd"] = json!({"area_id":if case == "wrong-area" { &a["id"] } else { &other["id"] },"folder_id":if case == "wrong-folder" { json!(99999) } else { folder["id"].clone() }})
            }
            "omitted-cross" => {
                request.as_object_mut().unwrap().remove("cwd");
                request.as_object_mut().unwrap().remove("isolation");
                request["allow_cross_area_cwd"] =
                    json!({"area_id":other["id"],"folder_id":folder["id"]});
            }
            "missing" => request["cwd"] = json!(fixture.root.path().join("missing")),
            "non-git" => {
                request["cwd"] = json!(bare);
                request["isolation"] = json!("worktree");
            }
            "relative" => request["cwd"] = json!("relative"),
            "owned" => request["cwd"] = json!(owned),
            "physical-owned" => request["cwd"] = json!(link),
            "actor" => {
                request["admin"] = json!(true);
            }
            _ => {}
        }
        let before = counts(&fixture).await;
        let status = if case == "actor" {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/tracks")
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("Idempotency-Key", case)
                        .header("X-Calm-Actor", "ai:planner")
                        .body(Body::from(request.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        } else {
            create(&app, &cookie, case, request).await.0
        };
        assert!(!status.is_success(), "{case}: {status}");
        assert_eq!(counts(&fixture).await, before, "{case}");
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8b_canonical_none_original_ack_survives_missing_directory() {
    for human in [false, true] {
        let fixture = Fixture::new().await;
        let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
        let a = area(&app, &cookie, "vanished").await;
        let path = repo(&fixture);
        let mut request = borrowed(&a, "codex", &path, "none");
        if human {
            request["first_message"] = json!(FIRST_BODY);
        }
        let first = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
        let track = first["id"].as_str().unwrap();
        let original = fixture.carrier(track).await;
        let worker_id = original.1.clone().into();
        let worker = fixture.repo.session_get(&worker_id).await.unwrap();
        let receipt = if human {
            Some(fixture.receipt(a["id"].as_str().unwrap()).await)
        } else {
            None
        };
        let before = counts(&fixture).await;
        std::fs::remove_dir_all(&path).unwrap();
        assert_eq!(
            post(&app, &cookie, "/api/tracks", "first", request.clone()).await,
            first
        );
        assert!(!create(&app, &cookie, "fresh", request).await.0.is_success());
        for suffix in ["", "-raw"] {
            assert!(
                !http(
                    &app,
                    &cookie,
                    "GET",
                    &format!("/api/tracks/{track}/workspace/readfile{suffix}?path=marker.svg")
                )
                .await
                .0
                .is_success()
            );
        }
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let (status, source) = crate::auth::authenticated_source(auth, headers).await;
        assert_eq!(status, StatusCode::OK);
        let route = crate::state::RouteState::from_ref(&state);
        let fence = crate::operation::planner_start_fence::CardStartFence::lock(
            &route.planner_recovery_locks,
            &route.repo,
            &route.operation_runtime,
            &original.0.clone().into(),
        )
        .await;
        let (_, mut peer) = connect(&state.shared_codex_appserver).await;
        assert!(state.shared_codex_appserver.is_running());
        crate::harness::backend::factory::PlannerFactory::new(
            Arc::downgrade(&state.shared_codex_appserver),
            &route.claude_planner,
            &route.acp_planner,
        )
        .ready_retained(&crate::session_projection_repo::AgentProvider::Codex)
        .await
        .unwrap();
        let wake = fence.wake_retained(
            route.planner_start_adapter.clone(),
            source.unwrap(),
            route.workspace_root.clone(),
            route.events.clone(),
        );
        tokio::select! {
            result = wake => assert!(result.is_err()),
            call = super::request(&mut peer) => panic!("missing directory allowed native crossing: {call}"),
            _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("missing directory wake hung"),
        }
        let errors: Vec<String> = sqlx::query_scalar(concat!(
            "SELECT compensation_state FROM operations WHERE kind='planner-harness-start' ",
            "AND json_extract(payload_json,'$.card_id')=?1"
        ))
        .bind(&original.0)
        .fetch_all(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(errors.len(), 1, "{errors:?}");
        // The retained owner deliberately redacts provider/resource diagnostics.
        // Its phase pins the failure after ready admission; mutation must prove
        // removing the missing-resource check instead reaches the native peer.
        let held: Value = serde_json::from_str(&errors[0]).unwrap();
        assert_eq!(held["from_phase"], "app_server_interact");
        assert_eq!(held["reason"], "retained activation held");
        assert_eq!(fixture.repo.session_get(&worker_id).await.unwrap(), worker);
        assert!(state.harness.get(&original.1).is_none());
        assert_eq!(fixture.carrier(track).await, original);
        if let Some(receipt) = receipt {
            assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
        }
        assert_eq!(counts(&fixture).await, before);
        assert!(!path.exists());
        shutdown(&fixture, &state).await;
    }
}
