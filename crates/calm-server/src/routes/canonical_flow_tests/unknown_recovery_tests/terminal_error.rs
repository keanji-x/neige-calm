//! Real native error ordering through authenticated canonical HTTP admission.
use super::*;

#[tokio::test]
async fn canonical_codex_terminal_error_declares_explicit_recovery() {
    Box::pin(terminal_error("fresh")).await;
}
#[tokio::test]
async fn canonical_codex_terminal_error_recovers_sticky_native_error() {
    Box::pin(terminal_error("sticky-error")).await;
}
#[tokio::test]
async fn canonical_codex_terminal_error_survives_kernel_restart() {
    Box::pin(terminal_error("cold")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_revoked_source() {
    Box::pin(terminal_error("revoke")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_backend_namespace() {
    Box::pin(terminal_error("backend")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_current_thread() {
    Box::pin(terminal_error("thread")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_current_carrier() {
    Box::pin(terminal_error("carrier")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_missing_terminal() {
    Box::pin(terminal_error("missing-terminal")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_missing_durable_terminal() {
    Box::pin(terminal_error("missing-durable")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_unconfirmed_cleanup() {
    Box::pin(terminal_error("unconfirmed")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_active_native() {
    Box::pin(terminal_error("active")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_workspace() {
    Box::pin(terminal_error("workspace")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_survives_native_reconnect() {
    Box::pin(terminal_error("reconnect")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_native_source() {
    Box::pin(terminal_error("native-source")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_another_activity_mirror() {
    Box::pin(terminal_error("other-active-mirror")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_sanitizes_native_refusals() {
    for scenario in ["read-refused", "resume-refused"] {
        Box::pin(terminal_error(scenario)).await;
    }
}

#[tokio::test]
async fn canonical_codex_terminal_error_settles_original_content_before_retirement() {
    Box::pin(terminal_error("content-pending")).await;
}

#[tokio::test]
async fn canonical_codex_terminal_error_refuses_incomplete_terminal_items() {
    for scenario in ["missing-items", "summary-items"] {
        Box::pin(terminal_error(scenario)).await;
    }
}

#[tokio::test]
async fn canonical_codex_terminal_error_rechecks_concurrent_track_event() {
    Box::pin(terminal_error("event")).await;
}

// Recovery cleanup joins the old actor. Always unblock it, including on panic.
struct ReleaseOwnerOnDrop(crate::test_seams::PausePoint);
impl Drop for ReleaseOwnerOnDrop {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}

async fn terminal_error(scenario: &str) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Terminal error","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "initial", json!({"area_id":area["id"],"title":"","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let track = track.as_str();
    let call = super::super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, _) = fixture.carrier(track).await;
    send(&mut peer, json!({"method":"thread/status/changed","params":{"threadId":THREAD,"status":{"type":"systemError"}}})).await;
    fixture.wait_phase(track, HarnessPhaseTag::Wedged).await;
    let content_pending = scenario == "content-pending";
    let end_probe = crate::test_seams::PausePoint::default();
    let owner_held = crate::test_seams::PausePoint::default();
    let _release_owner = ReleaseOwnerOnDrop(owner_held.clone());
    let original_item = json!({"id":"original-partial","type":"agentMessage","text":"Original partial reply retained before recovery"});
    if content_pending {
        for (key, text) in [("alpha", "Queued alpha"), ("beta", "Queued beta")] {
            post(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/input"),
                key,
                json!({"text":text}),
            )
            .await;
        }
        crate::test_seams::install_pause_for_test(
            "canonical-before-end-probe",
            &card,
            end_probe.clone(),
        );
        tokio::time::timeout(Duration::from_secs(5), end_probe.entered.notified())
            .await
            .unwrap();
        crate::test_seams::install_pause_for_test(
            "canonical-tick-returned",
            &card,
            owner_held.clone(),
        );
        send(&mut peer,json!({"method":"item/completed","params":{"threadId":THREAD,"turnId":TURN,"item":original_item}})).await;
    }
    notify_thread(&state.shared_codex_appserver, &mut peer, THREAD, "turn/completed", json!({"id":TURN,"status":"failed","error":{"codexErrorInfo":"server_overloaded","message":"Selected model is at capacity. Please try a different model."},"items":[]})).await;
    if content_pending {
        end_probe.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), owner_held.entered.notified())
            .await
            .unwrap();
    }
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    if !content_pending {
        for (key, text) in [("alpha", "Queued alpha"), ("beta", "Queued beta")] {
            post(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/input"),
                key,
                json!({"text":text}),
            )
            .await;
        }
        // Input acceptance commits before the old actor publishes its feedback
        // event. Establish a settled owner for success; the event-race scenario
        // below separately verifies that later durable ingress refuses recovery.
        crate::test_seams::install_pause_for_test(
            "canonical-tick-returned",
            &card,
            owner_held.clone(),
        );
        tokio::time::timeout(Duration::from_secs(5), owner_held.entered.notified())
            .await
            .unwrap();
    }
    let before = fixture.carrier(track).await.2;
    assert!(before.canonical_inputs.active.is_none());
    let mirrored_turn: Option<String> =
        sqlx::query_scalar("SELECT active_turn_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert!(
        mirrored_turn.is_none(),
        "known END must close factual activity without restoring authority"
    );
    assert_eq!(before.canonical_inputs.pending.len(), 2);
    assert_eq!(before.header.wedged_reason.as_deref(), Some("system_error"));
    let run = super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
    assert_eq!(run["runtime_state"], "failed");
    assert_eq!(
        run["recovery"]["action"], "recover_session",
        "known failed END must offer an explicit Human recovery"
    );

    let request_body = run["recovery"]["request"].clone();
    let uri = format!("/api/cards/{card}/conversation/recover");
    let history = history_rows(&fixture, &card, THREAD).await;
    let (state, app, cookie, _client, mut peer) = if scenario == "cold" {
        // Reopen the released known-END frame, whose old active-only mirror
        // writer left the ended native turn behind on its Failed row.
        sqlx::query("UPDATE worker_sessions SET active_turn_id=?1 WHERE id=?2")
            .bind(TURN)
            .bind(&worker)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let cold = fixture.root.path().join("cold-terminal.db");
        sqlx::query("VACUUM INTO ?1")
            .bind(cold.to_str().unwrap())
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        owner_held.release.notify_one();
        super::super::native_attempt_recovery_tests::shutdown(&state, &worker, &mut peer).await;
        peer.close(None).await.unwrap();
        fixture.shutdown_process(&state).await;
        drop((state, app, peer, _client));
        fixture.url = format!("sqlite:{}", cold.display());
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        let (state, app, cookie) = fixture.application().await;
        let (client, peer) = connect(&state.shared_codex_appserver).await;
        let reopened =
            super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
        assert_eq!(reopened["recovery"], run["recovery"]);
        (state, app, cookie, client, peer)
    } else if scenario == "reconnect" {
        peer.close(None).await.unwrap();
        drop((_client, peer));
        let (client, peer) = connect(&state.shared_codex_appserver).await;
        (state, app, cookie, client, peer)
    } else {
        (state, app, cookie, _client, peer)
    };
    assert_eq!(
        status(&app, "", &uri, "denied", request_body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    let mut stale = request_body.clone();
    stale["revision"] = json!("18446744073709551615");
    assert_eq!(
        status(&app, &cookie, &uri, "stale", stale).await,
        StatusCode::CONFLICT
    );
    if scenario == "other-active-mirror" {
        sqlx::query("UPDATE worker_sessions SET active_turn_id='different-turn' WHERE id=?1")
            .bind(&worker)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            status(&app, &cookie, &uri, "recover", request_body).await,
            StatusCode::CONFLICT
        );
        assert_eq!(fixture.carrier(track).await.2, before);
        owner_held.release.notify_one();
        fixture.shutdown_process(&state).await;
        return;
    }
    if scenario == "missing-durable" {
        sqlx::query(
            "DELETE FROM harness_items WHERE worker_session_id=?1 AND method='turn/completed'",
        )
        .bind(&worker)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(
            status(&app, &cookie, &uri, "recover", request_body).await,
            StatusCode::CONFLICT
        );
        assert_eq!(fixture.carrier(track).await.2, before);
        owner_held.release.notify_one();
        fixture.shutdown_process(&state).await;
        return;
    }
    let races = matches!(
        scenario,
        "revoke" | "backend" | "thread" | "carrier" | "workspace" | "native-source" | "event"
    );
    let pause = crate::test_seams::PausePoint::default();
    if races {
        crate::test_seams::install_pause_for_test(
            "canonical-abandon-before-commit",
            &card,
            pause.clone(),
        );
    }
    let (a, c, u, q) = (
        app.clone(),
        cookie.clone(),
        uri.clone(),
        request_body.clone(),
    );
    let mut action = tokio::spawn(async move {
        let response = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(u)
                    .header(header::COOKIE, c)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("Idempotency-Key", "recover")
                    .body(Body::from(q.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        (status, body)
    });
    let mut replacement_native = None;
    let mut deferred_start = None;
    let mut owner_released = false;
    let response = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            tokio::select! {
                result = &mut action => break result.unwrap(),
                _ = async {
                    loop {
                        if fixture.carrier(track).await.1 != worker { break; }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }, if scenario != "cold" && (!races || scenario == "event") && !owner_released => {
                    let rows = history_rows(&fixture,&card,THREAD).await;
                    let count = rows.iter().filter(|(_,_,body)|body.contains("Original partial reply retained before recovery")).count();
                    owner_released = true;
                    owner_held.release.notify_one();
                    if content_pending {
                        assert_eq!(count,1,"all original terminal content must be durable before retiring its event owner");
                    }
                }
                _ = pause.entered.notified(), if races => {
                    match scenario {
                        "event" => {
                            let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db()).await.unwrap();
                            append_pending_event(&mut tx, track, area["id"].as_str().unwrap()).await;
                            tx.commit().await.unwrap();
                        }
                        "native-source" => { replacement_native = Some(connect(&state.shared_codex_appserver).await); }
                        "revoke" => { post(&app,&cookie,"/api/auth/logout","logout",json!({})).await; }
                        "backend" => { super::codex::bind_other_backend(&fixture,&worker).await; }
                        "thread" => { sqlx::query("UPDATE worker_sessions SET thread_id='changed-thread' WHERE id=?1").bind(&worker).execute(fixture.repo.pool()).await.unwrap(); }
                        "carrier" => { sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1").bind(&card).execute(fixture.repo.pool()).await.unwrap(); }
                        "workspace" => { sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(track).execute(fixture.repo.pool()).await.unwrap(); }
                        _ => unreachable!(),
                    }
                    pause.release.notify_one();
                }
                call = request(&mut peer) => {
                    if (scenario == "read-refused" && call["method"] == "thread/read")
                        || (scenario == "resume-refused" && call["method"] == "thread/resume") {
                        send(&mut peer, json!({"id":call["id"],"error":{"code":-32603,"message":"private-native-recovery-payload"}})).await;
                        continue;
                    }
                    let result = match call["method"].as_str().unwrap() {
                        "config/read" => json!({"config":{}}),
                        "thread/read" => {
                            assert_eq!(call["params"]["threadId"],THREAD);
                            let mut terminal=json!({"id":TURN,"status":"failed","error":{"message":"private native diagnostic"},"items":if content_pending{json!([original_item.clone()])}else{json!([])}});
                            terminal["itemsView"]=json!(if scenario=="summary-items"{"summary"}else{"full"});
                            if scenario=="missing-items" {terminal.as_object_mut().unwrap().remove("items");}
                            json!({"thread":{"id":THREAD,"status":{"type":if scenario=="active"{"active"}else if scenario=="cold"{"notLoaded"}else{"systemError"}},"turns":if scenario=="missing-terminal"{json!([])}else{json!([terminal])}}})
                        }
                        "thread/resume" => {
                            assert_eq!(call["params"]["threadId"], THREAD);
                            json!({"thread":{"id":THREAD,"status":{"type":if scenario=="unconfirmed"{"active"}else if scenario=="sticky-error"{"systemError"}else{"idle"}}}})
                        }
                        "turn/interrupt" => {
                            assert_ne!(fixture.carrier(track).await.1, worker, "cleanup must follow the committed replacement");
                            assert_eq!(call["params"]["threadId"], THREAD);
                            assert_eq!(call["params"]["turnId"], TURN);
                            json!({})
                        }
                        "thread/start" => {
                            assert_ne!(fixture.carrier(track).await.1, worker, "fresh activation must follow commit");
                            assert!(deferred_start.replace(call).is_none());
                            continue;
                        }
                        "thread/unsubscribe" => json!({}),
                        method => panic!("recovery must not submit business input before committing: {method}"),
                    };
                    send(&mut peer,json!({"id":call["id"],"result":result})).await;
                }
            }
        }
    }).await.expect("recovery must settle through native owner");
    owner_held.release.notify_one();
    if races
        || matches!(
            scenario,
            "missing-terminal"
                | "missing-items"
                | "summary-items"
                | "unconfirmed"
                | "active"
                | "read-refused"
                | "resume-refused"
        )
    {
        assert!(
            !response
                .1
                .to_string()
                .contains("private-native-recovery-payload"),
            "native refusal payload escaped canonical recovery: {:?}",
            response.1
        );
        assert_eq!(
            response.0,
            if scenario == "revoke" {
                StatusCode::UNAUTHORIZED
            } else if matches!(scenario, "read-refused" | "resume-refused") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::CONFLICT
            },
            "{response:?}"
        );
        let old: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&old).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        let receipts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM conversation_abandonments WHERE old_worker=?1",
        )
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(receipts, 0);
    } else {
        assert_eq!(response.0, StatusCode::OK, "{response:?}");
        let (_, fresh, after) = fixture.carrier(track).await;
        assert_ne!(fresh, worker);
        assert_eq!(response.1["carrier_id"], fresh);
        let old: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&old).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        assert_eq!(after.header.push_watermark, before.header.push_watermark);
        assert_eq!(
            after.canonical_inputs.pending,
            before.canonical_inputs.pending
        );
        let retained = history_rows(&fixture, &card, THREAD).await;
        if content_pending {
            assert_eq!(retained.len(), history.len() + 1);
            assert!(history.iter().all(|row| retained.contains(row)));
            assert_eq!(
                retained
                    .iter()
                    .filter(|(_, _, body)| body
                        .contains("Original partial reply retained before recovery"))
                    .count(),
                1
            );
        } else {
            assert_eq!(retained, history);
        }
        assert!(
            history
                .iter()
                .any(|(_, method, body)| method == "turn/completed"
                    && body.contains("failed")
                    && !body.contains("Selected model"))
        );
        assert_eq!(
            post(&app, &cookie, &uri, "recover", request_body.clone()).await,
            response.1
        );
        // Explicit recovery wakes ordinary source admission. Exercise the actual
        // fresh native carrier, without accepting or advancing inputs in the fixture.
        let mut bodies = Vec::new();
        let fresh_thread = "recovered-fresh-thread";
        for index in 0..2 {
            let next=tokio::time::timeout(Duration::from_secs(10),async {
                loop {
                    let call = if let Some(call) = deferred_start.take() { call } else { request(&mut peer).await };
                    let result=match call["method"].as_str().unwrap() {
                        "turn/start" => break call,
                        "thread/start" => json!({"thread":{"id":fresh_thread}}),
                        "config/read" => json!({"config":{}}),
                        "thread/read" => json!({"thread":{"id":fresh_thread,"status":{"type":"idle"},"turns":[]}}),
                        "thread/unsubscribe" => json!({}),
                        method=>panic!("unexpected fresh dispatch request: {method}"),
                    };
                    send(&mut peer,json!({"id":call["id"],"result":result})).await;
                }
            }).await.expect("saved Human inputs must progress through ordinary current-source admission");
            assert_eq!(next["params"]["threadId"], fresh_thread);
            let body = next["params"]["input"].to_string();
            assert!(!body.contains(FIRST_BODY), "failed original was resent");
            bodies.push(body);
            let turn = format!("fresh-{index}");
            send(
                &mut peer,
                json!({"id":next["id"],"result":{"turn":{"id":turn}}}),
            )
            .await;
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                fresh_thread,
                "turn/started",
                json!({"id":turn,"status":"inProgress"}),
            )
            .await;
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                fresh_thread,
                "turn/completed",
                json!({"id":turn,"status":"completed","items":[]}),
            )
            .await;
            if bodies.iter().any(|body| body.contains("Queued beta")) {
                break;
            }
        }
        let joined = bodies.join("\n");
        assert_eq!(joined.matches("Queued alpha").count(), 1);
        assert_eq!(joined.matches("Queued beta").count(), 1);
        assert!(joined.find("Queued alpha") < joined.find("Queued beta"));
        let drained = fixture
            .wait_phase(track, HarnessPhaseTag::TurnCompleted)
            .await;
        assert!(drained.canonical_inputs.pending.is_empty());
        assert!(drained.canonical_inputs.active.is_none());
        assert_eq!(
            status(&app, &cookie, &uri, "different-key", request_body).await,
            StatusCode::CONFLICT
        );
    }
    drop(replacement_native);
    fixture.shutdown_process(&state).await;
}
