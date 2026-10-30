//! Real HTTP, protocol peer and file-DB reopen for the init-only identity window.
use super::*;
use crate::routes::canonical_flow_tests::assistant_canonical::carrier;
use crate::routes::canonical_flow_tests::claude_creation_tests::assistant::{
    configure, read, wait_frame,
};

#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_preserves_unknown_without_resend() {
    Box::pin(recover("fresh")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_after_keep() {
    Box::pin(recover("keep")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_preserves_pending_human() {
    Box::pin(recover("pending")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_refuses_unproven_quiescence() {
    Box::pin(recover("unproven")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_rechecks_human_source() {
    Box::pin(recover("revoke")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_rechecks_launch_owner() {
    Box::pin(recover("owner")).await;
}
#[tokio::test]
async fn canonical_claude_unconfirmed_session_abandon_refuses_worker_bound_pending() {
    Box::pin(recover("system")).await;
}

#[tokio::test]
async fn canonical_claude_unconfirmed_session_retry_retains_its_worker_boundary() {
    Box::pin(recover("retry")).await;
}

fn resolution_request(worker: &str, frame: &CanonicalHarnessSnapshot) -> Value {
    let attempt = frame.canonical_inputs.attempts.last().unwrap();
    json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,
        "attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()})
}

async fn recover(scenario: &str) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "no-input-replay");
    let (state, app, cookie) = fixture.application().await;
    sqlx::query("CREATE TRIGGER fail_native_bind BEFORE UPDATE OF agent_session_id ON worker_sessions WHEN NEW.agent_session_id IS NOT NULL BEGIN SELECT RAISE(ABORT,'bind failed'); END")
        .execute(fixture.repo.pool()).await.unwrap();
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Claude identity recovery","color":"#123456"}),
    )
    .await;
    let created = post(&app, &cookie, "/api/tracks", "initial", json!({"area_id":area["id"],"title":"","planner_provider":"claude","model":"claude-fable-5-1[1m]","reasoning_effort":"high","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let card = fixture.carrier(&track).await.0;
    let before = wait_frame(&fixture, &card, |frame| {
        frame
            .canonical_inputs
            .attempts
            .first()
            .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
    })
    .await;
    let (worker, _) = carrier(&fixture, &card).await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        carrier(&fixture, &card).await.1.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    let spawns = read(&fixture, "spawns");
    let stdin = read(&fixture, "stdin");
    assert_eq!(spawns.lines().count(), 1);
    let old_thread = before.header.last_thread_id.as_deref().unwrap();
    let history = history_rows(&fixture, &card, old_thread).await;
    if scenario == "keep" {
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/conversation/input/keep"),
            "keep",
            resolution_request(&worker, &before),
        )
        .await;
        wait_frame(&fixture, &card, |f| !f.canonical_inputs.receipts.is_empty()).await;
    }
    if scenario == "retry" {
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/conversation/input/retry"),
            "retry",
            resolution_request(&worker, &before),
        )
        .await;
        wait_frame(&fixture, &card, |f| !f.canonical_inputs.receipts.is_empty()).await;
    }
    if scenario == "pending" {
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "pending",
            json!({"text":"Pending Human retained across recovery"}),
        )
        .await;
    }
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
    let current = carrier(&fixture, &card).await.1;
    let body = resolution_request(&worker, &current);
    let uri = format!("/api/cards/{card}/conversation/abandon");
    assert_eq!(
        status(&app, "", &uri, "anonymous", body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    let mut stale = body.clone();
    stale["revision"] = json!("18446744073709551615");
    assert_eq!(
        status(&app, &cookie, &uri, "stale", stale).await,
        StatusCode::CONFLICT
    );
    let run = super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
    let worker_bound = current
        .canonical_inputs
        .pending
        .iter()
        .any(|p| !matches!(p, calm_types::conversation::PendingInput::Human { .. }));
    if scenario == "system" || (scenario == "retry" && worker_bound) {
        assert_eq!(run["recovery"]["action"], "held");
        assert_eq!(run["recovery"]["reason"], "pending_inputs");
        assert_eq!(
            status(&app, &cookie, &uri, "blocked", body).await,
            StatusCode::CONFLICT
        );
    } else {
        assert_eq!(run["recovery"]["action"], "abandon_session", "{run}");
        assert_eq!(run["recovery"]["request"], body);
        if scenario == "unproven" {
            add_unproven_execution(&fixture, &worker, &track).await;
            assert_eq!(
                status(&app, &cookie, &uri, "unproven", body).await,
                StatusCode::CONFLICT
            );
        } else if matches!(scenario, "revoke" | "owner") {
            let pause = crate::test_seams::PausePoint::default();
            crate::test_seams::install_pause_for_test(
                "canonical-abandon-before-commit",
                &card,
                pause.clone(),
            );
            let (a, c, u, b) = (app.clone(), cookie.clone(), uri.clone(), body.clone());
            let task = tokio::spawn(async move { status(&a, &c, &u, "abandon", b).await });
            tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
                .await
                .unwrap();
            if scenario == "revoke" {
                post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
            } else {
                sqlx::query("UPDATE claude_session_launches SET track_id='wrong-owner' WHERE worker_session_id=?1").bind(&worker).execute(fixture.repo.pool()).await.unwrap();
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
        } else {
            let fresh = post(&app, &cookie, &uri, "abandon", body.clone()).await;
            assert_ne!(fresh["carrier_id"], worker);
            assert_eq!(post(&app, &cookie, &uri, "abandon", body).await, fresh);
            let (fresh_worker, frame) = carrier(&fixture, &card).await;
            assert_ne!(fresh_worker, worker);
            assert_eq!(frame.header.phase, HarnessPhaseTag::Idle);
            assert!(frame.header.last_thread_id.is_none());
            assert!(frame.canonical_inputs.active.is_none());
            assert!(frame.canonical_inputs.attempts.is_empty());
            assert_eq!(
                frame.canonical_inputs.pending,
                current.canonical_inputs.pending
            );
            let retained: String =
                sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                    .bind(&worker)
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            let retained: Value = serde_json::from_str(&retained).unwrap();
            assert_eq!(
                retained["canonical_inputs"],
                serde_json::to_value(&current.canonical_inputs).unwrap()
            );
            assert_eq!(
                read(&fixture, "spawns"),
                spawns,
                "recovery starts no native process"
            );
            assert_eq!(
                read(&fixture, "stdin"),
                stdin,
                "uncertain input is never resent"
            );
            assert_eq!(history_rows(&fixture, &card, old_thread).await, history);
            std::fs::write(fixture.root.path().join("fake/scenario"), "exit").unwrap();
            post(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/input"),
                "fresh-human",
                json!({"text":"Authorized input on a fresh session"}),
            )
            .await;
            let after = wait_frame(&fixture, &card, |f| {
                f.canonical_inputs
                    .attempts
                    .iter()
                    .any(|a| matches!(a.delivery, InputDeliveryFact::Accepted { .. }))
            })
            .await;
            assert_ne!(after.header.last_thread_id.as_deref(), Some(old_thread));
            assert_eq!(
                read(&fixture, "stdin")
                    .lines()
                    .filter(|line| line.contains(FIRST_BODY))
                    .count(),
                1
            );
        }
    }
    fixture.shutdown_process(&state).await;
}

async fn add_unproven_execution(fixture: &Fixture, worker: &str, track: &str) {
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
        track,
        &metadata.alias.0,
        &serde_json::to_string(&metadata).unwrap(),
        "{}",
    )
    .await
    .unwrap();
    let settled = serde_json::to_string(&workspace_runtime::execution::Settlement::Journal(
        workspace_runtime::execution::ExecutionState::OriginalGroupEmpty { status: Some(0) },
    ))
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
}
