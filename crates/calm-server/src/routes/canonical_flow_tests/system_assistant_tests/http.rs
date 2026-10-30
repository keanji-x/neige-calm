use super::*;
use crate::track_activity::report_completion_for_test::{self, ReportCompletion};
use calm_truth::db::{RepoEventWrite, RepoRead};

async fn summary_post(app: &axum::Router, cookie: &str, key: &str) -> Value {
    post(app, cookie, "/api/today/summary", key, Value::Null).await
}

#[tokio::test]
async fn http_summary_same_key_replays_original_acceptance() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "first").await;
    let card = accepted["card_id"].as_str().unwrap();
    let original = recovery::receipts(&fixture, card).await;
    assert_eq!(original.len(), 2);
    let before = frame(&fixture, card).await;
    edit(&app, &cookie, &track, 1).await;
    assert_eq!(summary_post(&app, &cookie, "first").await, accepted);
    assert_eq!(recovery::receipts(&fixture, card).await, original);
    assert_eq!(frame(&fixture, card).await, before);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_summary_native_bootstrap_and_full_counts_freeze_until_new_key() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "first").await;
    let card = accepted["card_id"].as_str().unwrap();
    let original = recovery::receipts(&fixture, card).await;
    assert_eq!(original.len(), 2);
    let (worker, _) = frame(&fixture, card).await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let bootstrap = next(&mut peer, true).await;
    edit(&app, &cookie, &track, 1).await;
    assert_eq!(summary_post(&app, &cookie, "first").await, accepted);
    assert_eq!(recovery::receipts(&fixture, card).await, original);
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "http-bootstrap")
        .await;
    let first = next(&mut peer, false).await;
    native
        .complete(&mut peer, first, &body(1), "http-first")
        .await;
    phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(summary_post(&app, &cookie, "second").await, accepted);
    let second = next(&mut peer, false).await;
    native
        .complete(&mut peer, second, &body(2), "http-second")
        .await;
    let done = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(done.canonical_inputs.attempts.len(), 3);
    assert_eq!(frame(&fixture, card).await.0, worker);
    let receipts = recovery::receipts(&fixture, card).await;
    assert_eq!(receipts.len(), 3);
    assert_eq!(&receipts[..2], original.as_slice());
    assert_eq!(summary_post(&app, &cookie, "first").await, accepted);
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(frame(&fixture, card).await.1, done);
    let (status, fresh) = response(&app, &cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fresh, accepted);
    let call = next(&mut peer, false).await;
    native
        .complete(&mut peer, call, &body(2), "http-unkeyed")
        .await;
    phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(recovery::receipts(&fixture, card).await.len(), 4);
    for receipt in &original {
        sqlx::query("DELETE FROM events WHERE id=?1")
            .bind(receipt.event_id)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM harness_items WHERE card_id=?1")
        .bind(card)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(summary_post(&app, &cookie, "first").await, accepted);
    assert_eq!(
        summary_post(&app, &cookie, "after-deletion").await,
        accepted
    );
    let call = next(&mut peer, false).await;
    native
        .complete(&mut peer, call, &body(2), "http-after-deletion")
        .await;
    let done = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(done.canonical_inputs.attempts.len(), 5);
    assert_eq!(frame(&fixture, card).await.0, worker);
    let receipts = recovery::receipts(&fixture, card).await;
    assert_eq!(receipts.len(), 5);
    assert_eq!(&receipts[..2], original.as_slice());
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_summary_concurrent_keys_on_independent_connections_keep_every_body() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&state, &app, &cookie).await;
    fixture.data_dir = Some(fixture.root.path().join("peer-data"));
    let (other, other_app, other_cookie) = fixture.application().await;
    let (first, duplicate) = tokio::join!(
        summary_post(&app, &cookie, "first"),
        summary_post(&other_app, &other_cookie, "first")
    );
    assert_eq!(first, duplicate);
    let card = first["card_id"].as_str().unwrap();
    assert_eq!(recovery::receipts(&fixture, card).await.len(), 2);
    let (second, third) = tokio::join!(
        summary_post(&app, &cookie, "second"),
        summary_post(&other_app, &other_cookie, "third")
    );
    assert_eq!(second, first);
    assert_eq!(third, first);
    let receipts = recovery::receipts(&fixture, card).await;
    assert_eq!(receipts.len(), 4);
    let (worker, held) = frame(&fixture, card).await;
    assert_eq!(held.canonical_inputs.pending.len(), 4);
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.original_worker == worker)
    );
    other.dispatcher.retire();
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let bootstrap = next(&mut peer, true).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "http-race-bootstrap")
        .await;
    for turn in ["http-race-one", "http-race-two", "http-race-three"] {
        let call = next(&mut peer, false).await;
        native.complete(&mut peer, call, &body(1), turn).await;
    }
    let done = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(done.canonical_inputs.attempts.len(), 4);
    assert_eq!(frame(&fixture, card).await.0, worker);
    assert_eq!(recovery::receipts(&fixture, card).await, receipts);
    assert_eq!(fixture.counts(card).await.2, 1);
    shutdown(&fixture, &state).await;
    shutdown(&fixture, &other).await;
}

#[path = "../../../../tests/cases/today_launchpad_snapshot.rs"]
mod database_snapshot;

async fn response(app: &axum::Router, cookie: &str, key: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/today/summary")
        .header(header::COOKIE, cookie);
    if let Some(key) = key {
        builder = builder.header("Idempotency-Key", key);
    }
    let reply = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = reply.status();
    let body =
        serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}

// Observe the actual boot projectors' committed products before whole-DB
// assertions. No timed sleep or omitted table can stand in for their completion.
async fn boot_projectors_ready(fixture: &Fixture, state: &AppState) {
    let tracks: Vec<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE closed_at IS NULL")
        .fetch_all(fixture.repo.pool())
        .await
        .unwrap();
    let mut events = state.events.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut ready = true;
            for track in &tracks {
                let overlays = fixture.repo.overlays_for("track", track).await.unwrap();
                ready &= [
                    crate::track_activity::ACTIVITY_OVERLAY_KIND,
                    crate::track_planner_context::PLANNER_CONTEXT_OVERLAY_KIND,
                    crate::track_planner_context::RUN_STATS_OVERLAY_KIND,
                ]
                .into_iter()
                .all(|kind| overlays.iter().any(|overlay| overlay.kind == kind));
            }
            if ready {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn projected_report(
    fixture: &Fixture,
    track: &str,
    ready: &mut tokio::sync::broadcast::Receiver<ReportCompletion>,
) {
    let event = fixture
        .repo
        .events_for_track(track, &["track.report_edited"], None)
        .await
        .unwrap()
        .into_iter()
        .max_by_key(|event| event.id)
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        report_completion_for_test::wait_for_report(ready, &event),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn activity_settled(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
) -> String {
    let route = crate::state::RouteState::from_ref(state);
    let mut ready = route
        .activity_wake
        .subscribe_report_completions_for_test()
        .unwrap();
    let track = activity(state, app, cookie).await;
    projected_report(fixture, &track, &mut ready).await;
    track
}
#[tokio::test]
async fn http_empty_and_drift_refusals_preserve_every_database_value() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    boot_projectors_ready(&fixture, &state).await;
    let before = database_snapshot::database(&fixture.repo).await;
    for key in [None, Some("empty")] {
        let (status, body) = response(&app, &cookie, key).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "today_summary_no_activity");
        assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    }
    activity_settled(&fixture, &state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "original").await;
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let saved = frame(&fixture, &card).await;
    shutdown(&fixture, &state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((state, app, cookie, repo));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    fixture.workspace_root = Some(fixture.root.path().join("different-configured-root"));
    let (state, app, cookie) = fixture.application().await;
    boot_projectors_ready(&fixture, &state).await;
    let before = database_snapshot::database(&fixture.repo).await;
    assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
    assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    let (status, body) = response(&app, &cookie, Some("new")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "today_adoption_workspace_drift");
    assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    assert_eq!(frame(&fixture, &card).await, saved);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_failed_deleted_and_empty_activity_replay_is_permanent_read_only_evidence() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let activity_track = activity_settled(&fixture, &state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "original").await;
    let card = accepted["card_id"].as_str().unwrap();
    let receipts = recovery::receipts(&fixture, card).await;
    let (worker, saved) = frame(&fixture, card).await;
    // Change the contributing Track's actual visibility, without editing any
    // already accepted event, to exercise a genuinely empty activity projection.
    sqlx::query("UPDATE tracks SET area_id=(SELECT area_id FROM tracks WHERE purpose='launchpad') WHERE id=?1")
        .bind(&activity_track).execute(fixture.repo.pool()).await.unwrap();
    for mutation in [
        "UPDATE worker_sessions SET state='failed' WHERE id=?1",
        "DELETE FROM cards WHERE session_id=?1",
    ] {
        sqlx::query(mutation)
            .bind(&worker)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let before = database_snapshot::database(&fixture.repo).await;
        assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
        assert_eq!(database_snapshot::database(&fixture.repo).await, before);
        assert_eq!(recovery::receipts(&fixture, card).await, receipts);
        let (status, body) = response(&app, &cookie, Some("new")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "today_summary_no_activity");
        assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    }
    let raw: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<CanonicalHarnessSnapshot>(&raw).unwrap(),
        saved
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_thread_start_after_send_error_keeps_original_ack_and_held_claim_after_reopen() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "original").await;
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let original = recovery::receipts(&fixture, &card).await;
    let (worker, _) = frame(&fixture, &card).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let operation = loop {
        let call = request(&mut peer).await;
        match call["method"].as_str().unwrap() {
            "config/read" => send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await,
            "thread/start" => {
                let op: String =
                    sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
                        .bind(&worker)
                        .fetch_one(fixture.repo.pool())
                        .await
                        .unwrap();
                // A real native request crossed the socket. An error response
                // carries no BeforeSend proof and may not release/re-mint it.
                send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"native thread allocation outcome unknown"}})).await;
                break op;
            }
            method => panic!("unexpected pre-activation RPC {method}"),
        }
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        state.operation_runtime.wait(&operation),
    )
    .await
    .unwrap()
    .unwrap();
    let held: Option<String> =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(held.as_deref(), Some(operation.as_str()));
    let saved = frame(&fixture, &card).await;
    assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await, saved);
    assert!(matches!(
        state
            .dispatcher
            .bootstrap_canonical_child(&card.clone().into(), &worker)
            .await,
        Err(crate::error::CalmError::Conflict(_))
    ));
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fixture, &state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((client, peer, state, app, cookie, repo));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await, saved);
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT spawn_op_id FROM worker_sessions WHERE id=?1"
        )
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap(),
        held
    );
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let call = native_request(&mut peer).await;
    assert_eq!(call["method"], "config/mcpServer/reload");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    state.dispatcher.reconcile_tick_for_test().await;
    assert!(matches!(
        state
            .dispatcher
            .bootstrap_canonical_child(&card.clone().into(), &worker)
            .await,
        Err(crate::error::CalmError::Conflict(_))
    ));
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await, saved);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_file_reopen_replays_original_key_and_new_key_uses_original_worker() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let activity_track = activity(&state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "original").await;
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let original = recovery::receipts(&fixture, &card).await;
    let saved = frame(&fixture, &card).await;
    shutdown(&fixture, &state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((state, app, cookie, repo));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
    assert_eq!(frame(&fixture, &card).await, saved);
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    edit(&app, &cookie, &activity_track, 1).await;
    assert_eq!(summary_post(&app, &cookie, "new").await, accepted);
    assert_eq!(frame(&fixture, &card).await.0, saved.0);
    let receipts = recovery::receipts(&fixture, &card).await;
    assert_eq!(receipts.len(), 3);
    assert_eq!(&receipts[..2], original.as_slice());
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card: &card,
    };
    let bootstrap = next(&mut peer, true).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "reopen-bootstrap")
        .await;
    for (turn, expected) in [("reopen-original", body(1)), ("reopen-new", body(2))] {
        let call = next(&mut peer, false).await;
        native.complete(&mut peer, call, &expected, turn).await;
    }
    let done = phase(&fixture, &card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(done.canonical_inputs.attempts.len(), 3);
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(frame(&fixture, &card).await.0, saved.0);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn http_human_and_historical_namespace_refuse_before_legacy_adoption_even_after_card_delete()
{
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    use crate::routes::track_conversations::*;
    for historical in [false, true] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application_with_historical_native(historical).await;
        boot_projectors_ready(&fixture, &state).await;
        activity_settled(&fixture, &state, &app, &cookie).await;
        let launchpad = post(
            &app,
            &cookie,
            "/api/today/launchpad/ensure",
            "ensure",
            Value::Null,
        )
        .await;
        let track = launchpad["track_id"].as_str().unwrap();
        let card = crate::conversation_keys::derive_track_conversation_keys(
            track,
            calm_types::system_assistant::SystemAssistantSource::TodaySummary.namespace(),
        )
        .card_id;
        if historical {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("Idempotency-Key", "today-summary".parse().unwrap());
            create_track_conversation_inner(
                crate::state::RouteState::from_ref(&state),
                crate::state::WorkerState::from_ref(&state),
                crate::actor::Actor::server_send(),
                headers,
                track.into(),
                NewTrackConversationBody {
                    provider: AssistantProviderRequest::LegacyCodex,
                    side: None,
                    text: "historical sentinel".into(),
                    model: None,
                    reasoning_effort: None,
                },
                OpeningBriefing::CallerSuppliesItsOwn,
                AssistantCreationEntry::InternalHistorical,
            )
            .await
            .unwrap();
        } else {
            let created = post(
                &app,
                &cookie,
                &format!("/api/tracks/{track}/conversations"),
                "today-summary",
                json!({"provider":"codex","text":"Human sentinel"}),
            )
            .await;
            assert_eq!(created["id"], card);
        }
        let worker: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
        if let Some(handle) = state.harness.remove(&worker) {
            handle.shutdown().await.unwrap();
        }
        if let Some(sweep) = &state.provider_sweep {
            sweep.stop().await.unwrap();
        }
        sqlx::query("UPDATE tracks SET purpose=NULL,template_id='small-change',plugin_scope='legacy' WHERE id=?1").bind(track).execute(fixture.repo.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER refuse_namespace_adoption BEFORE UPDATE OF purpose ON tracks BEGIN SELECT RAISE(ABORT,'namespace must be qualified before adoption'); END").execute(fixture.repo.pool()).await.unwrap();
        for deleted in [false, true] {
            if deleted {
                sqlx::query("DELETE FROM cards WHERE id=?1")
                    .bind(&card)
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            let before = database_snapshot::database(&fixture.repo).await;
            let (status, body) = response(&app, &cookie, Some("new")).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["code"], "idempotency_key_exhausted");
            assert_eq!(database_snapshot::database(&fixture.repo).await, before);
            assert!(recovery::receipts(&fixture, &card).await.is_empty());
        }
        shutdown(&fixture, &state).await;
    }
}

#[tokio::test]
async fn http_canonical_reset_refuses_and_old_key_remains_read_only_original_ack() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    boot_projectors_ready(&fixture, &state).await;
    activity_settled(&fixture, &state, &app, &cookie).await;
    let accepted = summary_post(&app, &cookie, "original").await;
    let card = accepted["card_id"].as_str().unwrap();
    let original = recovery::receipts(&fixture, card).await;
    let worker = frame(&fixture, card).await.0;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let bootstrap = next(&mut peer, true).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "before-reset-bootstrap")
        .await;
    let body_call = next(&mut peer, false).await;
    let mut completed = state.events.subscribe();
    let mut projected = crate::state::RouteState::from_ref(&state)
        .activity_wake
        .subscribe_report_completions_for_test()
        .unwrap();
    native
        .complete(&mut peer, body_call, &body(1), "before-reset-body")
        .await;
    // Frame commit precedes phase publication. Observe the final envelope and its
    // actual activity-owner completion, including unchanged projections.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let env = completed.recv().await.unwrap();
            if matches!(&env.event, crate::event::Event::HarnessPhaseChanged {
                card_id, new_phase: HarnessPhaseTag::TurnCompleted, ..
            } if card_id.as_str() == card)
            {
                report_completion_for_test::wait_for_event(&mut projected, &env)
                    .await
                    .unwrap();
                break;
            }
        }
    })
    .await
    .unwrap();
    let settled = frame(&fixture, card).await;
    assert_eq!(settled.0, worker);
    let reset_uri = format!("/api/cards/{card}/planner/reset");
    boot_projectors_ready(&fixture, &state).await;
    let before = database_snapshot::database(&fixture.repo).await;
    let reply = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(reset_uri)
                .header(header::COOKIE, &cookie)
                .header("Idempotency-Key", "reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::CONFLICT);
    let error: Value =
        serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(error["code"], "conflict");
    assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    assert_eq!(frame(&fixture, card).await, settled);
    assert_eq!(recovery::receipts(&fixture, card).await, original);
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    let before = database_snapshot::database(&fixture.repo).await;
    assert_eq!(summary_post(&app, &cookie, "original").await, accepted);
    assert_eq!(database_snapshot::database(&fixture.repo).await, before);
    assert_eq!(recovery::receipts(&fixture, card).await, original);
    assert_eq!(frame(&fixture, card).await, settled);
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fixture, &state).await;
}
