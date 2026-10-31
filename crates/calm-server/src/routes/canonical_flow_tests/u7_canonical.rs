//! U7 exercises authenticated HTTP admission, not an alternate allocation fixture.
use super::*;

#[cfg(test)]
#[tokio::test]
async fn u7_canonical_named_sources_commit_original_human_acceptance() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        state.plugin.enable("gitforge").await.unwrap();
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"U7", "color":"#123456"}),
        )
        .await;
        let recipe = post(
            &app,
            &cookie,
            "/api/track-recipes",
            "recipe",
            json!({"title":"Frozen recipe", "body":"# Frozen recipe\n\nOriginal method."}),
        )
        .await;
        let source = post(
            &app,
            &cookie,
            "/api/tracks",
            "source",
            json!({"area_id":area["id"], "title":"source", "planner_provider":provider, "first_message":"Source pending author queue",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}}),
        )
        .await;
        let (source_card, source_worker, source_frame) =
            fixture.carrier(source["id"].as_str().unwrap()).await;
        assert_eq!(
            http(
                &app,
                &cookie,
                "PUT",
                &format!("/api/cards/{source_card}/planner/permission-mode"),
                json!({"permission_mode":"full"})
            )
            .await
            .0,
            StatusCode::OK
        );
        for (field, value, input) in [
            ("template_id", json!("small-change"), None),
            ("recipe_id", recipe["id"].clone(), None),
            (
                "template_id",
                json!("dev"),
                Some(json!({"merge_policy":"ask"})),
            ),
            ("fork_report_from", source["id"].clone(), None),
        ] {
            let mut body = json!({"area_id":area["id"], "title":"U7", "planner_provider":provider,
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}, "first_message":FIRST_BODY});
            body[field] = value;
            if let Some(input) = input {
                body["template_input"] = input;
            }
            let key = format!("{field}-{}", body.get("template_input").is_some());
            let created = post(&app, &cookie, "/api/tracks", &key, body.clone()).await;
            let (card, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
            assert_eq!(frame.canonical_inputs.pending.len(), 1);
            assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
            let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
                .bind(&card)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
            let payload: Value = serde_json::from_str(&payload).unwrap();
            let context: calm_types::conversation::SystemRenderContext = serde_json::from_value(
                payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY].clone(),
            )
            .unwrap();
            if field == "fork_report_from" {
                assert!(context.text().contains("## Creation-time forked Report"));
                assert!(!context.text().contains("Source pending author queue"));
                let frozen: Value =
                    serde_json::from_str(&context.text()[context.text().find('{').unwrap()..])
                        .unwrap();
                let report: String = sqlx::query_scalar(
                    "SELECT payload FROM cards WHERE track_id=?1 AND role='reportcard'",
                )
                .bind(created["id"].as_str().unwrap())
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
                let report: Value = serde_json::from_str(&report).unwrap();
                assert_eq!(frozen["body"], report["body"]);
                assert_eq!(frozen["summary"], report["summary"]);
                let (_, child_worker, _) = fixture.carrier(created["id"].as_str().unwrap()).await;
                assert_ne!(child_worker, source_worker);
                assert_eq!(
                    fixture
                        .carrier(source["id"].as_str().unwrap())
                        .await
                        .2
                        .canonical_inputs,
                    source_frame.canonical_inputs
                );
                assert_ne!(
                    frame.canonical_inputs.pending, source_frame.canonical_inputs.pending,
                    "fork accepts its own author/message; it never copies the source queue"
                );
                let facts: (Option<String>, Option<String>, Option<String>, String) = sqlx::query_as(
                    "SELECT w.thread_id,w.agent_session_id,w.mcp_token_hash,json_extract(c.payload,'$.permission_mode')
                    FROM worker_sessions w JOIN cards c ON c.id=w.card_id WHERE w.id=?1")
                    .bind(&child_worker).fetch_one(fixture.repo.pool()).await.unwrap();
                assert_eq!(facts, (None, None, None, "never".into()));
            }

            let mut tx = fixture.repo.pool().begin().await.unwrap();
            let receipt = crate::db::sqlite::conversation::human_track_creation_get_tx(
                &mut tx,
                area["id"].as_str().unwrap(),
                &key,
            )
            .await
            .unwrap();
            assert!(
                receipt.is_some(),
                "atomic original receipt for {provider} {field}"
            );
            tx.rollback().await.unwrap();
            if field == "fork_report_from" {
                assert!(
                    http(
                        &app,
                        &cookie,
                        "DELETE",
                        &format!("/api/tracks/{}", source["id"].as_str().unwrap()),
                        Value::Null
                    )
                    .await
                    .0
                    .is_success()
                );
            }
            assert_eq!(
                post(&app, &cookie, "/api/tracks", &key, body).await["id"],
                created["id"]
            );
        }
        assert!(!state.shared_codex_appserver.is_running());
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn u7_canonical_empty_template_retains_first_send_without_starting_provider() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"U7 empty", "color":"#123456"}),
        )
        .await;
        let body = json!({"area_id":area["id"], "title":"U7", "planner_provider":provider,
            "template_id":"small-change", "theme":{"fg":[255,255,255],"bg":[0,0,0]}});
        let created = post(&app, &cookie, "/api/tracks", "empty", body.clone()).await;
        let track = created["id"].as_str().unwrap();
        let (card, worker, empty) = fixture.carrier(track).await;
        assert!(empty.canonical_inputs.pending.is_empty());
        assert!(empty.canonical_inputs.active.is_none());
        assert!(empty.canonical_inputs.attempts.is_empty());
        assert_eq!(fixture.counts(&card).await, (0, 0, 1, 0, 0));
        assert!(!state.shared_codex_appserver.is_running());
        assert!(state.harness.get(&worker).is_none());
        assert_eq!(
            post(&app, &cookie, "/api/tracks", "empty", body).await["id"],
            created["id"]
        );
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "send",
            json!({"text":FIRST_BODY}),
        )
        .await;
        let (same_card, same_worker, sent) = fixture.carrier(track).await;
        assert_eq!((same_card, same_worker), (card.clone(), worker));
        assert_eq!(sent.canonical_inputs.pending.len(), 1);
        assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
        fixture.shutdown_process(&state).await;
    }
}

async fn http(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn u7_canonical_original_recipe_receipt_replays_after_source_deletion() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"replay", "color":"#123456"}),
        )
        .await;
        let recipe = post(
            &app,
            &cookie,
            "/api/track-recipes",
            "recipe",
            json!({"title":"Original method", "body":"# Method\nOriginal bytes."}),
        )
        .await;
        let body = json!({"area_id":area["id"],"planner_provider":provider,"title":"",
            "recipe_id":recipe["id"],"first_message":FIRST_BODY,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
        let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
        let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
        let before = fixture.carrier(created["id"].as_str().unwrap()).await;
        let uri = format!("/api/track-recipes/{}", recipe["id"].as_str().unwrap());
        assert_eq!(
            http(
                &app,
                &cookie,
                "PUT",
                &uri,
                json!({"title":"Changed", "body":"# Changed", "if_revision":1})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(
            http(&app, &cookie, "DELETE", &uri, Value::Null)
                .await
                .0
                .is_success()
        );
        assert_eq!(
            post(&app, &cookie, "/api/tracks", "first", body.clone()).await["id"],
            created["id"]
        );
        assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
        assert_eq!(
            fixture
                .carrier(created["id"].as_str().unwrap())
                .await
                .2
                .canonical_inputs,
            before.2.canonical_inputs
        );
        assert_eq!(
            status(&app, &cookie, "/api/tracks", "fresh", body).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(fixture.counts(&before.0).await, (1, 1, 1, 0, 0));
        fixture.shutdown_process(&state).await;
    }
}

#[cfg(feature = "fixtures")]
#[tokio::test]
#[allow(deprecated)] // Exercise the existing leaf deletion owner and its cache contract.
async fn u7_canonical_fork_source_deletion_and_access_revocation_races_rollback() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        for revoke in [false, true] {
            let fixture = Fixture::new().await;
            let (state, app, cookie) = fixture.application().await;
            let area = post(
                &app,
                &cookie,
                "/api/areas",
                "area",
                json!({"name":"race", "color":"#123456"}),
            )
            .await;
            let base = json!({"area_id":area["id"],"planner_provider":provider,"title":"", "theme":{"fg":[255,255,255],"bg":[0,0,0]}});
            let source = post(&app, &cookie, "/api/tracks", "source", {
                let mut source = base.clone();
                source["first_message"] = json!("Source pending author queue");
                source
            })
            .await;
            let mut body = base;
            body["fork_report_from"] = source["id"].clone();
            body["first_message"] = json!(FIRST_BODY);
            let pause = PausePoint::default();
            install_pause_for_test(
                "track-canonical-allocation",
                area["id"].as_str().unwrap(),
                pause.clone(),
            );
            let creating = status(&app, &cookie, "/api/tracks", "racing", body);
            tokio::pin!(creating);
            tokio::select! {
                _ = pause.entered.notified() => {},
                result = &mut creating => panic!("allocation must await authoritative admission: {result}"),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing allocation rendezvous"),
            }
            if revoke {
                sqlx::query("DELETE FROM area_members WHERE area_id=?1")
                    .bind(area["id"].as_str().unwrap())
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            } else {
                // The create holds the target Area fence, so simulate the already-admitted
                // source deletion in its storage owner, rather than deadlock its HTTP fence.
                let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
                    .await
                    .unwrap();
                crate::db::sqlite::track_delete_tx(
                    &mut tx,
                    source["id"].as_str().unwrap(),
                    state.write().area_cache(),
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            pause.release.notify_one();
            assert_eq!(
                creating.await,
                if revoke {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::BAD_REQUEST
                }
            );
            let accepted: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM conversation_human_track_creations WHERE area_id=?1 AND idempotency_key='racing'",
            )
            .bind(area["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
            assert_eq!(accepted, 0);
            let bindings: i64 = sqlx::query_scalar("SELECT count(*) FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key='racing'")
                .bind(area["id"].as_str().unwrap()).fetch_one(fixture.repo.pool()).await.unwrap();
            assert_eq!(bindings, 0);
            fixture.shutdown_process(&state).await;
        }
    }
}

#[cfg(test)]
async fn stored_plan(
    fixture: &Fixture,
    frame: &CanonicalHarnessSnapshot,
) -> crate::db::sqlite::conversation::StoredSubmissionPlan {
    let id = &frame
        .canonical_inputs
        .active
        .as_ref()
        .unwrap()
        .submission_ids[0];
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let plan = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    plan
}

#[tokio::test]
async fn u7_canonical_frozen_context_binds_first_plan_once_across_cold_restart() {
    use calm_types::conversation::{PrivateHistoryPayload, SubmissionRenderSegment};
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"frozen", "color":"#123456"}),
    )
    .await;
    let recipe = post(&app, &cookie, "/api/track-recipes", "recipe", json!({"title":"Original frozen method", "body":"# Method\nFrozen U7 sentinel {track_id}."})).await;
    let body = json!({"area_id":area["id"],"planner_provider":"codex","title":"",
        "recipe_id":recipe["id"],"first_message":FIRST_BODY,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
    super::u7_nondelivery::hold(&fixture).await;
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, _) = fixture.carrier(&track).await;
    let recipe_uri = format!("/api/track-recipes/{}", recipe["id"].as_str().unwrap());
    assert_eq!(
        http(
            &app,
            &cookie,
            "PUT",
            &recipe_uri,
            json!({"title":"Changed method", "body":"# Changed U7 sentinel", "if_revision":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    super::u7_nondelivery::release_twice(&fixture, &app, &cookie, &track, &body, Some(&mut peer))
        .await;
    sqlx::query(&format!("CREATE TRIGGER hold_u7_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN NEW.id='{worker}' AND json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts')>json_array_length(OLD.handle_state_json,'$.canonical_inputs.attempts')
        BEGIN SELECT RAISE(ABORT,'hold U7 first crossing'); END"))
        .execute(fixture.repo.pool()).await.unwrap();
    post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let claimed = super::source_readmission_tests::wait_for_claim_after_attempts(
        &fixture, &track, &mut peer, 2,
    )
    .await;
    let original = stored_plan(&fixture, &claimed).await;
    let contexts = original
        .plan
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            SubmissionRenderSegment::Context {
                payload: PrivateHistoryPayload::Present { value },
            } if value.text().contains("Frozen U7 sentinel") => Some(value.text()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        contexts.len(),
        1,
        "exactly one creation Context in the actual first claim"
    );
    assert!(contexts[0].contains("{track_id}"));
    assert!(
        !serde_json::to_string(&original.plan)
            .unwrap()
            .contains("Changed U7 sentinel")
    );
    assert_eq!(
        original.plan.input_refs().unwrap().len(),
        1,
        "context is never Human input"
    );
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
    peer.close(None).await.unwrap();
    state
        .harness
        .remove(&worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    fixture.shutdown_process(&state).await;
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        stored_plan(&fixture, &fixture.carrier(&track).await.2).await,
        original
    );
    sqlx::query("DROP TRIGGER hold_u7_marker")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", body).await["id"],
        created["id"]
    );
    let first = next_turn(&mut peer).await;
    assert_eq!(
        first["params"]["input"]
            .to_string()
            .matches("Frozen U7 sentinel")
            .count(),
        1
    );
    assert!(
        !first["params"]["input"]
            .to_string()
            .contains("Changed U7 sentinel")
    );
    assert_eq!(
        stored_plan(&fixture, &fixture.carrier(&track).await.2).await,
        original
    );
    complete(&state, &mut peer, &first, TURN).await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
        .await;
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"Follow up without injecting the creation context again"}),
    )
    .await;
    let second = next_turn(&mut peer).await;
    assert!(
        !second["params"]["input"]
            .to_string()
            .contains("Frozen U7 sentinel")
    );
    let next_plan = stored_plan(&fixture, &fixture.carrier(&track).await.2).await;
    assert!(
        !serde_json::to_string(&next_plan.plan)
            .unwrap()
            .contains("Frozen U7 sentinel")
    );
    complete(&state, &mut peer, &second, "u7-second-turn").await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
        .await;
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
    peer.close(None).await.unwrap();
    if let Some(handle) = state.harness.remove(&worker) {
        handle.shutdown().await.unwrap();
    }
    fixture.shutdown_process(&state).await;
}

async fn next_turn(peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/read" => {
                    json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
                }
                "thread/start" | "thread/resume" => {
                    assert!(
                        !call["params"].to_string().contains("Frozen U7 sentinel"),
                        "creation context belongs to the frozen plan, not session instructions"
                    );
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "turn/start" => break call,
                method => panic!("unexpected protocol call: {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("production must dispatch the original retained carrier")
}

async fn complete(
    state: &AppState,
    peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer,
    call: &Value,
    turn: &str,
) {
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
}

#[tokio::test]
async fn u7_canonical_original_template_key_survives_catalog_removal_after_reopen() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let mut fixture = Fixture::new().await;
        let templates = fixture.root.path().join("templates");
        std::fs::create_dir(&templates).unwrap();
        let file = templates.join("method.md");
        let header = calm_types::report_contract::canonical_line(
            &calm_types::track_report::work_brief_header(),
        );
        std::fs::write(&file, format!("+++\nid = \"method\"\ntitle = \"Frozen method\"\n+++\n{header}\n# Original catalog bytes\n")).unwrap();
        fixture.templates_dir = Some(templates);
        let (state, app, cookie) = fixture.application().await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"catalog", "color":"#123456"}),
        )
        .await;
        let body = json!({"area_id":area["id"],"planner_provider":provider,"title":"",
            "template_id":"site/method", "first_message":FIRST_BODY,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
        let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
        let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
        fixture.shutdown_process(&state).await;
        drop((state, app));
        std::fs::remove_file(file).unwrap();
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        let (state, app, cookie) = fixture.application().await;
        assert_eq!(
            post(&app, &cookie, "/api/tracks", "first", body.clone()).await["id"],
            created["id"]
        );
        assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
        assert_eq!(
            status(&app, &cookie, "/api/tracks", "fresh", body).await,
            StatusCode::BAD_REQUEST
        );
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
                .bind(created["id"].as_str().unwrap())
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert!(payload.contains("Original catalog bytes"));
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn u7_canonical_receipt_failure_rolls_back_all_named_allocations() {
    for provider in calm_types::runtime::AgentProvider::ALL.map(|provider| provider.wire_name()) {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        state.plugin.enable("gitforge").await.unwrap();
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"atomic", "color":"#123456"}),
        )
        .await;
        let recipe = post(
            &app,
            &cookie,
            "/api/track-recipes",
            "recipe",
            json!({"title":"Atomic method", "body":"# Method\nNever partially allocate."}),
        )
        .await;
        let mut body = json!({"area_id":area["id"],"planner_provider":provider,"title":"",
            "first_message":FIRST_BODY,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
        let source = post(&app, &cookie, "/api/tracks", "source", body.clone()).await;
        sqlx::query(
            "CREATE TRIGGER fail_u7_receipt BEFORE INSERT ON conversation_human_track_creations
            BEGIN SELECT RAISE(ABORT,'fixture refuses U7 receipt'); END",
        )
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        let before = allocation_counts(&fixture).await;
        for (field, value, input) in [
            ("template_id", json!("small-change"), None),
            ("recipe_id", recipe["id"].clone(), None),
            (
                "template_id",
                json!("dev"),
                Some(json!({"merge_policy":"ask"})),
            ),
            ("fork_report_from", source["id"].clone(), None),
        ] {
            body[field] = value;
            if let Some(input) = input {
                body["template_input"] = input;
            }
            assert_eq!(
                status(&app, &cookie, "/api/tracks", "refused", body.clone()).await,
                StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(
                allocation_counts(&fixture).await,
                before,
                "receipt failure rolls back Track, cards, V2, binding, Human message and presentation: {provider} {field}"
            );
            body.as_object_mut().unwrap().remove(field);
            body.as_object_mut().unwrap().remove("template_input");
        }
        fixture.shutdown_process(&state).await;
    }
}

async fn allocation_counts(fixture: &Fixture) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),
        (SELECT count(*) FROM worker_sessions),(SELECT count(*) FROM track_create_idempotency),
        (SELECT count(*) FROM conversation_messages),(SELECT count(*) FROM conversation_presentations)")
        .fetch_one(fixture.repo.pool()).await.unwrap()
}
