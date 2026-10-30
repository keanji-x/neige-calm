//! Unknown recovery exercises production HTTP admission and the real ACP process owner.
use super::*;
use calm_truth::db::RepoOutOfDomain;
mod claude;
mod codex;
mod stop_timeout;
mod terminal_error;

#[tokio::test]
async fn canonical_acp_cold_unknown_explicit_abandon_starts_fresh_without_resend() {
    Box::pin(acp_recovery("fresh")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_abandon_preserves_pending_human_identity() {
    Box::pin(acp_recovery("pending")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_abandon_refuses_unproven_quiescence() {
    Box::pin(acp_recovery("unproven")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_abandon_rechecks_human_source() {
    Box::pin(acp_recovery("revoke")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_abandon_rechecks_registration() {
    Box::pin(acp_recovery("registration")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_abandon_rechecks_card_contract() {
    Box::pin(acp_recovery("ownership")).await;
}
#[tokio::test]
async fn canonical_acp_cold_unknown_keep_consumes_genuine_normal_join() {
    Box::pin(acp_recovery("keep")).await;
}

#[tokio::test]
async fn canonical_acp_cold_unknown_concurrent_abandon_replays_one_receipt() {
    Box::pin(acp_recovery("concurrent")).await;
}

#[tokio::test]
async fn canonical_acp_assistant_abandon_keeps_planner_root() {
    Box::pin(acp_recovery("assistant")).await;
}
#[tokio::test]
async fn canonical_acp_abandon_refuses_worker_bound_system_observation() {
    Box::pin(acp_recovery("system")).await;
}
#[tokio::test]
async fn canonical_acp_retry_refusal_preserves_unknown_recovery() {
    Box::pin(acp_recovery("retry")).await;
}
#[tokio::test]
async fn canonical_acp_abandon_does_not_skip_unaccepted_event_tail() {
    Box::pin(acp_recovery("watermark")).await;
}

fn acp_requests(fixture: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(fixture.root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn acp_recovery(scenario: &str) {
    let mut fixture = Fixture::new().await;
    opencode_creation_tests::configure(&mut fixture);
    std::fs::write(fixture.root.path().join("scenario"), "lost").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Unknown recovery","color":"#123456"}),
    )
    .await;
    let mut create = json!({"area_id":area["id"],"title":"","planner_provider":"opencode","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
    if scenario == "assistant" {
        create.as_object_mut().unwrap().remove("first_message");
    }
    if scenario == "retry" {
        create["model"] = json!("fixture/model-a");
        create["reasoning_effort"] = json!("normal");
    }
    let created = post(&app, &cookie, "/api/tracks", "initial", create).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let card = if scenario == "assistant" {
        post(
            &app,
            &cookie,
            &format!("/api/tracks/{track}/conversations"),
            "assistant",
            json!({"provider":"opencode","text":FIRST_BODY}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        fixture.carrier(&track).await.0
    };
    let (card, worker, _) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let facts = acp_carrier(&fixture, &card).await;
            if facts
                .2
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
            {
                break facts;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if scenario == "pending" {
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "queued",
            json!({"text":"Accepted pending Human before fresh session"}),
        )
        .await;
    }
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let mut token_tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let old_token =
        crate::mcp_server::wiring::mint_and_persist_card_token(&mut token_tx, &card, &worker)
            .await
            .unwrap();
    token_tx.commit().await.unwrap();
    let token_params = json!({"protocolVersion":"2024-11-05","clientInfo":{"name":"recovery-test","version":"1"},"_meta":{"dev.neige/auth":{"token":old_token}}});
    assert!(
        crate::mcp_server::handshake::handle_initialize(
            state.repo.as_ref(),
            None,
            &token_params,
            "2024-11-05"
        )
        .await
        .is_ok()
    );
    if scenario == "system" {
        accept_worker_bound_observation(
            &fixture,
            &card,
            &worker,
            &track,
            area["id"].as_str().unwrap(),
        )
        .await;
    }
    if scenario == "retry" {
        let current = acp_carrier(&fixture, &card).await.2;
        let a = &current.canonical_inputs.attempts[0];
        assert_eq!(status(&app,&cookie,&format!("/api/cards/{card}/conversation/input/retry"),"retry",json!({"carrier_id":worker,"submission_id":a.attempt.submission_id,"attempt_id":a.attempt.attempt_id,"revision":a.revision.to_string()})).await,StatusCode::CONFLICT);
        assert_eq!(
            acp_carrier(&fixture, &card).await.2.canonical_inputs,
            current.canonical_inputs
        );
    }
    let before = acp_carrier(&fixture, &card).await.2;
    let root_before: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    if scenario == "watermark" {
        state.dispatcher.retire();
        let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
            .await
            .unwrap();
        append_pending_event(&mut tx, &track, area["id"].as_str().unwrap()).await;
        tx.commit().await.unwrap();
    }

    let attempt = &before.canonical_inputs.attempts[0];
    let request = json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()});
    let receipt_before: (String, String, Option<String>) = sqlx::query_as(
        "SELECT input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    let old_thread = before.header.last_thread_id.as_deref().unwrap();
    let history_before = history_rows(&fixture, &card, old_thread).await;
    let uri = format!("/api/cards/{card}/conversation/abandon");
    assert_eq!(
        status(&app, "", &uri, "unauthorized", request.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    let mut stale = request.clone();
    stale["revision"] = json!("18446744073709551615");
    assert_eq!(
        status(&app, &cookie, &uri, "stale", stale).await,
        StatusCode::CONFLICT
    );
    if scenario == "system" {
        let run = super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
        assert_eq!(run["recovery"]["action"], "held");
        assert_eq!(
            status(&app, &cookie, &uri, "abandon", request.clone()).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            acp_carrier(&fixture, &card).await.2.canonical_inputs,
            before.canonical_inputs
        );
    } else if scenario == "keep" {
        let witnesses: i64 =
            sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?1")
                .bind(&worker)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(witnesses, 1, "normal join before Keep must be durable");
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/conversation/input/keep"),
            "keep",
            request.clone(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fixture
                    .carrier(&track)
                    .await
                    .2
                    .canonical_inputs
                    .active
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a new explicit Keep must consume a genuine prior join");
        assert_eq!(acp_carrier(&fixture, &card).await.1, worker);
    } else if matches!(scenario, "revoke" | "registration" | "ownership") {
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "canonical-abandon-before-commit",
            &card,
            pause.clone(),
        );
        let app_request = app.clone();
        let cookie_request = cookie.clone();
        let uri_request = uri.clone();
        let body = request.clone();
        let task = tokio::spawn(async move {
            status(&app_request, &cookie_request, &uri_request, "abandon", body).await
        });
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .unwrap();
        match scenario {
            "revoke" => {
                post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
            }
            "registration" => {
                sqlx::query("UPDATE acp_managed_sessions SET registration_digest='changed-registration' WHERE worker_session_id=?1").bind(&worker).execute(fixture.repo.pool()).await.unwrap();
            }
            "ownership" => {
                sqlx::query("UPDATE cards SET payload=json_set(payload,'$.model','changed/model') WHERE id=?1").bind(&card).execute(fixture.repo.pool()).await.unwrap();
            }
            _ => unreachable!(),
        }
        pause.release.notify_one();
        assert_eq!(
            task.await.unwrap(),
            if scenario == "revoke" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::CONFLICT
            }
        );
        assert_eq!(acp_carrier(&fixture, &card).await.1, worker);
    } else if scenario == "unproven" {
        let settled = serde_json::to_string(&workspace_runtime::execution::Settlement::Journal(
            workspace_runtime::execution::ExecutionState::OriginalGroupEmpty { status: Some(0) },
        ))
        .unwrap();
        // Settlements are write-once. Add a distinct group-only history record
        // through the execution store instead of rewriting the original proof.
        let original: String = sqlx::query_scalar(
            "SELECT intent_json FROM workspace_executions WHERE alias LIKE ?1 LIMIT 1",
        )
        .bind(format!("planner:{worker}:%"))
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        let mut metadata: workspace_runtime::execution::ExecutionMetadata =
            serde_json::from_str(&original).unwrap();
        let id = workspace_runtime::execution::ExecutionId::fresh().unwrap();
        metadata.id = id.clone();
        metadata.alias.0 = format!("planner:{worker}:{}", id.as_str());
        let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
            .await
            .unwrap();
        crate::db::sqlite::workspace_execution::insert(
            &mut tx,
            id.as_str(),
            &track,
            &metadata.alias.0,
            &serde_json::to_string(&metadata).unwrap(),
            "{}",
        )
        .await
        .unwrap();
        crate::db::sqlite::workspace_execution::settle_tx(
            &mut tx,
            id.as_str(),
            &settled,
            crate::model::now_ms(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        crate::db::sqlite::workspace_execution::finish(fixture.repo.db(), id.as_str())
            .await
            .unwrap();
        assert_eq!(
            status(&app, &cookie, &uri, "abandon", request.clone()).await,
            StatusCode::CONFLICT
        );
        assert_eq!(acp_carrier(&fixture, &card).await.1, worker);
    } else {
        let run = super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
        assert_eq!(run["recovery"]["action"], "abandon_session");
        assert_eq!(run["history_replacement_supported"], false);
        let mut events = state.events.subscribe();
        let response: Value = if scenario == "concurrent" {
            let first_pause = crate::test_seams::PausePoint::default();
            crate::test_seams::install_pause_for_test(
                "canonical-abandon-before-commit",
                &card,
                first_pause.clone(),
            );
            let (a, c, u, q) = (app.clone(), cookie.clone(), uri.clone(), request.clone());
            let first = tokio::spawn(async move { post(&a, &c, &u, "abandon", q).await });
            tokio::time::timeout(Duration::from_secs(10), first_pause.entered.notified())
                .await
                .unwrap();
            let second_pause = crate::test_seams::PausePoint::default();
            crate::test_seams::install_pause_for_test(
                "canonical-abandon-before-push",
                &card,
                second_pause.clone(),
            );
            let (a, c, u, q) = (app.clone(), cookie.clone(), uri.clone(), request.clone());
            let second = tokio::spawn(async move { post(&a, &c, &u, "abandon", q).await });
            tokio::time::timeout(Duration::from_secs(10), second_pause.entered.notified())
                .await
                .unwrap();
            second_pause.release.notify_one();
            first_pause.release.notify_one();
            let result = first.await.unwrap();
            assert_eq!(second.await.unwrap(), result);
            result
        } else {
            post(&app, &cookie, &uri, "abandon", request.clone()).await
        };
        let (_, fresh, frame) = acp_carrier(&fixture, &card).await;
        assert_ne!(fresh, worker);
        assert_eq!(response["carrier_id"], fresh);
        let refreshed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let envelope = events.recv().await.unwrap();
                if let crate::event::Event::HarnessPhaseChanged {
                    worker_session_id,
                    new_phase,
                    ..
                } = &envelope.event
                    && worker_session_id == &fresh
                    && *new_phase == HarnessPhaseTag::Idle
                {
                    break envelope;
                }
            }
        })
        .await
        .expect("committed recovery must invalidate other readers");
        assert!(refreshed.id > 0);

        assert_eq!(frame.header.push_watermark, before.header.push_watermark);
        assert_eq!(
            frame.canonical_inputs.pending,
            before.canonical_inputs.pending
        );
        assert_eq!(
            frame.canonical_inputs.next_queue_position,
            before.canonical_inputs.next_queue_position
        );
        assert!(frame.canonical_inputs.active.is_none());
        let retired: Value = sqlx::query_scalar::<_, String>(
            "SELECT handle_state_json FROM worker_sessions WHERE id=?1",
        )
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap();
        assert_eq!(retired, serde_json::to_value(&before).unwrap());
        let root: Option<String> =
            sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
                .bind(&track)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        if scenario == "assistant" {
            assert_eq!(root, root_before);
        } else {
            assert_eq!(root.as_deref(), Some(fresh.as_str()));
        }
        let credentials: i64 = sqlx::query_scalar(concat!(
            "SELECT (SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1)+(SELECT count(*) FROM ",
            "worker_sessions WHERE card_id=?1 AND mcp_token_hash IS NOT NULL)"
        ))
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(credentials, 0);
        assert!(
            crate::mcp_server::handshake::handle_initialize(
                state.repo.as_ref(),
                None,
                &token_params,
                "2024-11-05"
            )
            .await
            .is_err()
        );
        assert_eq!(
            post(&app, &cookie, &uri, "abandon", request.clone()).await,
            response
        );
        assert_eq!(
            status(&app, &cookie, &uri, "another-action", request.clone()).await,
            StatusCode::CONFLICT
        );
        let old_identity: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        std::fs::write(fixture.root.path().join("scenario"), "reply").unwrap();
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "fresh",
            json!({"text":"Fresh authorized Human input"}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let f = acp_carrier(&fixture, &card).await.2;
                if f.canonical_inputs.active.is_none()
                    && f.canonical_inputs.pending.is_empty()
                    && !f.canonical_inputs.attempts.is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let identity: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
                .bind(&fresh)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_ne!(identity.0, old_identity.0);
        assert_ne!(identity.1, old_identity.1);
        let calls = acp_requests(&fixture);
        let prompts: Vec<_> = calls
            .iter()
            .filter(|c| c["method"] == "session/prompt")
            .collect();
        let bodies: Vec<String> = prompts
            .iter()
            .map(|p| p["params"]["prompt"].to_string())
            .collect();
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.contains(FIRST_BODY))
                .count(),
            1,
            "uncertain original input was resent"
        );
        assert!(bodies[1..].iter().all(|body| !body.contains(FIRST_BODY)));
        let fresh_index = bodies
            .iter()
            .position(|body| body.contains("Fresh authorized Human input"))
            .unwrap();
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.contains("Fresh authorized Human input"))
                .count(),
            1
        );
        if scenario == "pending" {
            let pending_index = bodies
                .iter()
                .position(|body| body.contains("Accepted pending Human before fresh session"))
                .unwrap();
            assert_eq!(
                bodies
                    .iter()
                    .filter(|body| body.contains("Accepted pending Human before fresh session"))
                    .count(),
                1
            );
            assert!(
                pending_index <= fresh_index,
                "pending Human input order changed"
            );
            if pending_index == fresh_index {
                assert!(
                    bodies[fresh_index]
                        .find("Accepted pending Human before fresh session")
                        .unwrap()
                        < bodies[fresh_index]
                            .find("Fresh authorized Human input")
                            .unwrap()
                );
            }
        }
        state.harness.shutdown_and_remove(&fresh).await.unwrap();
    }
    let receipt_after: (String, String, Option<String>) = sqlx::query_as(
        "SELECT input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(receipt_after, receipt_before);
    let history_after = history_rows(&fixture, &card, old_thread).await;
    for row in history_before {
        assert!(
            history_after.contains(&row),
            "recovery rewrote existing history"
        );
    }
    if !matches!(
        scenario,
        "fresh" | "pending" | "concurrent" | "assistant" | "watermark" | "retry"
    ) {
        assert_eq!(
            acp_requests(&fixture)
                .iter()
                .filter(|c| c["method"] == "session/prompt")
                .count(),
            1
        );
    }
    if matches!(
        scenario,
        "fresh" | "pending" | "concurrent" | "assistant" | "watermark" | "retry" | "keep"
    ) {
        if let Some(handle) = state.harness.remove(&worker) {
            handle.shutdown().await.unwrap();
        }
        let deleted = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/tracks/{track}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = deleted.status();
        let body = deleted.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "evidence must not prevent explicit domain deletion: {}",
            String::from_utf8_lossy(&body)
        );
        let joins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?1")
                .bind(&worker)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        let abandonments: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM conversation_abandonments WHERE old_worker=?1",
        )
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!((joins, abandonments), (0, 0));
    }
    fixture.shutdown_process(&state).await;
}

async fn history_rows(fixture: &Fixture, card: &str, thread: &str) -> Vec<(i64, String, String)> {
    fixture
        .repo
        .transcript_rows_of_thread(card, thread)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.id, row.method, row.params))
        .collect()
}

async fn acp_carrier(fixture: &Fixture, card: &str) -> (String, String, CanonicalHarnessSnapshot) {
    let (worker, frame) = super::assistant_canonical::carrier(fixture, card).await;
    (card.into(), worker, frame)
}
async fn append_pending_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &str,
    area: &str,
) -> i64 {
    crate::db::sqlite::append_decision_event_in_tx(
        tx,
        &crate::ids::ActorId::Kernel,
        &crate::event::EventScope::Track {
            track: track.into(),
            area: area.into(),
        },
        None,
        &crate::event::Event::TrackWakeRequested {
            track_id: track.into(),
            source: "recovery-test".into(),
            key: "pending".into(),
            text: "Original worker-bound observation".into(),
        },
    )
    .await
    .unwrap()
}
async fn accept_worker_bound_observation(
    fixture: &Fixture,
    card: &str,
    worker: &str,
    track: &str,
    area: &str,
) {
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let id = append_pending_event(&mut tx, track, area).await;
    let event = crate::db::sqlite::event_inputs::event_input_tx(&mut tx, id)
        .await
        .unwrap();
    let mut carrier =
        crate::db::sqlite::conversation::current_input_carrier_tx(&mut tx, card, worker)
            .await
            .unwrap();
    let observation = calm_types::conversation::SystemObservation::try_from(
        crate::harness::Observation::TrackWake {
            source: "recovery-test".into(),
            key: "pending".into(),
            text: "Original worker-bound observation".into(),
        },
    )
    .unwrap();
    crate::db::sqlite::conversation::accept_pending_system_tx(
        &mut tx,
        &mut carrier,
        observation,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
