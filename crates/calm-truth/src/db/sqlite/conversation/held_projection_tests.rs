//! Actual protected carrier reads and boot conversion, without runtime handles.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture_card, plan};
use super::transaction_tests::{provenance, runtime};
use super::*;
use crate::db::RepoRead;
use calm_types::conversation::*;
use calm_types::harness::{PlannerPermissionMode, snapshot::CanonicalHarnessSnapshot};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use serde_json::{Value, json};

async fn held(repo: &SqlxRepo) -> (String, String, InputAttempt) {
    let (_, card) = fixture_card(repo).await;
    let worker = runtime(repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let accepted = accept_pending_human_tx(
        &mut tx,
        &mut carrier,
        LEGACY_OWNER_USER_ID,
        body("canonical held body"),
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human fixture")
    };
    let claimed = claim_initial_submission_tx(&mut tx, &mut carrier, plan(message), &provenance())
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: claimed.submission_id,
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    let correlation = InputCorrelationFact {
        provider: AgentProvider::Codex,
        thread_id: "thread".into(),
        native_session_id: None,
        native_turn_id: None,
        local_turn_id: None,
        rpc_request_id: None,
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation.clone(),
        FrozenInputInvocation {
            model: Some("original-model".into()),
            effort: Some("high".into()),
            permission: PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation,
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card, worker, attempt)
}

#[tokio::test]
async fn canonical_held_projection_survives_failed_and_dormant_reload() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("held.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let before = repo
        .conversation_read_frame(&card)
        .await
        .unwrap()
        .active_input
        .unwrap();
    let projected = &before.submissions[0];
    assert_eq!(projected.carrier_id, worker);
    assert_eq!(projected.submission_id, attempt.submission_id.as_str());
    assert_eq!(projected.attempt.as_ref().unwrap().revision, "1");
    assert_eq!(
        projected.attempt.as_ref().unwrap().attempt_id,
        attempt.attempt_id.as_str()
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    super::super::session_fail_if_active_runtime_tx(&mut tx, &worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(repo);
    let reopened = SqlxRepo::open(&url).await.unwrap();
    let frame = reopened.conversation_read_frame(&card).await.unwrap();
    assert_eq!(
        frame.runtime.unwrap().state,
        calm_types::worker::WorkerSessionState::Failed
    );
    assert_eq!(frame.active_input.unwrap(), before);
}

#[tokio::test]
async fn runtime_frame_cannot_erase_same_phase_attempt_revision() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, _) = held(&repo).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let stale = RuntimeExecutionFence::from_carrier(&carrier);
    let mut next = carrier.snapshot().clone();
    next["canonical_inputs"]["attempts"][0]["revision"] = json!(2);
    super::super::session_set_handle_state_tx(&mut tx, &worker, Some(next))
        .await
        .unwrap();
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &worker, &stale, stale.header().clone())
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
}

fn released(mut frame: Value) -> Value {
    frame["schema_version"] = json!(2);
    for attempt in frame["canonical_inputs"]["attempts"]
        .as_array_mut()
        .unwrap()
    {
        let attempt = attempt.as_object_mut().unwrap();
        attempt.remove("revision");
        attempt.remove("invocation");
    }
    frame
}

#[tokio::test]
async fn canonical_migration_open_converts_all_retained_states_and_preserves_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("upgrade.db").display()
    );
    let repo = SqlxRepo::open(&url).await.unwrap();
    let mut originals = vec![];
    for state in [
        "starting",
        "running",
        "idle",
        "turn_pending",
        "failed",
        "exited",
        "superseded",
    ] {
        let (card, worker, _) = held(&repo).await;
        let frame = repo.conversation_read_frame(&card).await.unwrap();
        let old = released(frame.snapshot.unwrap());
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?1,state=?2 WHERE id=?3")
            .bind(old.to_string())
            .bind(state)
            .bind(&worker)
            .execute(repo.pool())
            .await
            .unwrap();
        originals.push((worker, old, state));
    }
    // The historical body queue is never adopted or normalized.
    let (_, old_card) = fixture_card(&repo).await;
    let old_worker = runtime(&repo, &old_card).await;
    let historical =
        r#"{"schema_version":1,"mode":"harness","pending_queue":[{"text":"private legacy"}]}"#;
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(historical)
        .bind(&old_worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let (_, other_card) = fixture_card(&repo).await;
    let other_worker = runtime(&repo, &other_card).await;
    let other_format = r#"{"schema_version":2,"mode":"another_component","opaque":true}"#;
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(other_format)
        .bind(&other_worker)
        .execute(repo.pool())
        .await
        .unwrap();
    drop(repo);
    let reopened = SqlxRepo::open(&url).await.unwrap();
    let unchanged_other: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(other_worker)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(unchanged_other, other_format);
    for (worker, old, state) in originals {
        let (raw, actual_state): (String, String) =
            sqlx::query_as("SELECT handle_state_json,state FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(reopened.pool())
                .await
                .unwrap();
        let current: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(actual_state, state);
        assert_eq!(current["canonical_inputs"]["attempts"][0]["revision"], 0);
        assert_eq!(
            current["canonical_inputs"]["attempts"][0]["invocation"],
            json!({"kind":"prior_invocation_not_recorded"})
        );
        assert_eq!(released(current.clone()), old);
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(current).is_ok());
    }
    let unchanged: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(old_worker)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(unchanged, historical);
    // A released binary's known migration set refuses the rollback fence.
    let old_binary = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 177)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    assert!(
        super::super::infra::check_no_unknown_future_migrations(reopened.pool(), &old_binary)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn canonical_migration_refuses_malformed_and_future_atomically() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, _) = held(&repo).await;
    let old = released(
        repo.conversation_read_frame(&card)
            .await
            .unwrap()
            .snapshot
            .unwrap(),
    );
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(old.to_string())
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let (_, other) = fixture_card(&repo).await;
    let corrupt_worker = runtime(&repo, &other).await;
    for version in [3, 4] {
        let mut corrupt = old.clone();
        corrupt["schema_version"] = json!(version);
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
            .bind(corrupt.to_string())
            .bind(&corrupt_worker)
            .execute(repo.pool())
            .await
            .unwrap();
        assert!(
            super::super::canonical_input_revision_migration::migrate(repo.db())
                .await
                .is_err()
        );
        let raw: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), old);
    }
}
