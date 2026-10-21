//! Storage guards use receipt evidence produced by the actual resolution writer.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture};
use super::transaction_tests::{provenance, runtime};
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;
use sqlx::{Row, Sqlite, Transaction};

const SUCCESSOR: &str = "cccccccccccccccccccccccccccccccc";
const SUCCESSOR_TURN: &str = "dddddddddddddddddddddddddddddddd";
const ATTEMPT: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

pub(super) struct Seed {
    pub(super) repo: SqlxRepo,
    pub(super) card: String,
    pub(super) carrier: String,
    pub(super) root: InputSubmissionId,
    pub(super) human: MessageRevisionRef,
    pub(super) render: SubmissionRenderPlan,
}

pub(super) async fn seed_repo(repo: SqlxRepo, card: String, legacy: bool) -> Seed {
    seed_repo_invocation(
        repo,
        card,
        legacy,
        FrozenInputInvocation {
            model: Some("original-model".into()),
            effort: Some("high".into()),
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
    )
    .await
}

pub(super) async fn seed_repo_invocation(
    repo: SqlxRepo,
    card: String,
    legacy: bool,
    invocation: FrozenInputInvocation,
) -> Seed {
    seed_repo_payload(repo, card, legacy, invocation, body("frozen Human")).await
}

pub(super) async fn seed_repo_payload(
    repo: SqlxRepo,
    card: String,
    legacy: bool,
    invocation: FrozenInputInvocation,
    payload: HumanPayload,
) -> Seed {
    seed_repo_binding(
        repo,
        card,
        legacy,
        invocation,
        payload,
        (calm_types::runtime::AgentProvider::Codex, "thread"),
    )
    .await
}

pub(super) async fn seed_repo_binding(
    repo: SqlxRepo,
    card: String,
    legacy: bool,
    invocation: FrozenInputInvocation,
    payload: HumanPayload,
    (provider, thread): (calm_types::runtime::AgentProvider, &str),
) -> Seed {
    let carrier = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query(
        "UPDATE worker_sessions SET provider=?1, thread_id=?2,
        handle_state_json=json_set(handle_state_json, '$.last_thread_id', ?2) WHERE id=?3",
    )
    .bind(provider.wire_name())
    .bind(thread)
    .bind(&carrier)
    .execute(&mut **tx)
    .await
    .unwrap();
    let mut current = current_input_carrier_tx(&mut tx, &card, &carrier)
        .await
        .unwrap();
    let human = accept_pending_human_tx(&mut tx, &mut current, LEGACY_OWNER_USER_ID, payload)
        .await
        .unwrap();
    let AcceptedInputRef::Human { message: human } = human.input_ref else {
        panic!("Human")
    };
    let system = SystemObservation::try_from(calm_types::observation::Observation::SystemContext {
        text: "frozen System".into(),
    })
    .unwrap();
    let observation_id = SystemObservationId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::SystemObservation(&observation_id),
    )
    .await
    .unwrap();
    current.checkpoint.pending.push(PendingInput::System {
        observation_id,
        observation: system,
        queue_position: 1,
        queued_at_ms: 1,
        envelope_id: None,
        addressable: true,
    });
    current.checkpoint.next_queue_position = 2;
    super::carrier::save_input_carrier_tx(&mut tx, &mut current)
        .await
        .unwrap();
    let render = pending_plan_for_prefix_tx(
        &mut tx,
        &current,
        std::num::NonZeroU32::new(2).unwrap(),
        vec![
            SystemRenderContext::new(
                calm_types::model::HarnessInputPresentation::System,
                "frozen context".into(),
            )
            .unwrap(),
        ],
    )
    .await
    .unwrap();
    if legacy {
        // Seed the released inline schema, not the new claim writer. Migration
        // and repository reopen below are the production behavior under test.
        let root = InputSubmissionId::parse("11111111111111111111111111111111").unwrap();
        super::tests::submission(
            &mut tx,
            &card,
            root.as_str(),
            "22222222222222222222222222222222",
            &render,
        )
        .await;
        sqlx::query("INSERT INTO conversation_submission_inputs(submission_id, input_position, input_kind, message_id,
            revision, queue_position, queued_at_ms, effective) VALUES(?1, 0, 'human', ?2, 1, 0, 1,
            1)")
            .bind(root.as_str()).bind(human.message_id.as_str()).execute(&mut **tx).await.unwrap();
        sqlx::query(
            "INSERT INTO conversation_submission_inputs(submission_id, input_position, input_kind,
            observation_id, queue_position, queued_at_ms, system_addressable, effective) VALUES(?1,
            1, 'system', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 1, 1, 1, 1)",
        )
        .bind(root.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        return Seed {
            repo,
            card,
            carrier,
            root,
            human,
            render,
        };
    }
    let mut execution = provenance();
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        provider: original, ..
    } = &mut execution;
    *original = provider.clone();
    let claimed = claim_initial_submission_tx(&mut tx, &mut current, render.clone(), &execution)
        .await
        .unwrap();
    begin_input_attempt_tx(
        &mut tx,
        &mut current,
        InputAttempt {
            submission_id: claimed.submission_id.clone(),
            attempt_id: InputAttemptId::parse(ATTEMPT).unwrap(),
        },
        InputCorrelationFact {
            provider: provider.clone(),
            thread_id: thread.into(),
            native_turn_id: None,
            native_session_id: None,
            local_turn_id: None,
            rpc_request_id: None,
        },
        invocation,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Seed {
        repo,
        card,
        carrier,
        root: claimed.submission_id,
        human,
        render,
    }
}

pub(super) async fn seed() -> Seed {
    let (repo, _, card) = fixture().await;
    seed_repo(repo, card, false).await
}

// Roll back only the SQL effects to let guard tests stage invalid linkages.
// The checkpoint bytes are produced by the real transaction, never a fixture reducer.
async fn receipt(tx: &mut Transaction<'_, Sqlite>, s: &Seed) {
    sqlx::query("SAVEPOINT real_retry_evidence")
        .execute(&mut **tx)
        .await
        .unwrap();
    super::retry::resolve_retry_tx(
        tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "retry-key",
        &request(s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let raw: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&s.carrier)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    sqlx::query("ROLLBACK TO real_retry_evidence; RELEASE real_retry_evidence")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(raw)
        .bind(&s.carrier)
        .execute(&mut **tx)
        .await
        .unwrap();
}

pub(super) fn request(s: &Seed) -> KeepConversationInputRequest {
    KeepConversationInputRequest {
        carrier_id: s.carrier.clone(),
        submission_id: s.root.as_str().into(),
        attempt_id: ATTEMPT.into(),
        revision: "0".into(),
    }
}

async fn relation(
    tx: &mut Transaction<'_, Sqlite>,
    s: &Seed,
) -> sqlx::Result<sqlx::sqlite::SqliteQueryResult> {
    sqlx::query("INSERT INTO conversation_input_retry_relations(retry_intent_id,card_id,carrier_id,
        predecessor_submission_id,root_submission_id,predecessor_attempt_id,resolution_key,
        principal_user_id,fingerprint,queue_position,created_at_ms)
        SELECT json_extract(handle_state_json,'$.canonical_inputs.receipts[0].decision.retry_intent_id'),card_id,id,?2,?2,?3,'retry-key',?4,json_extract(handle_state_json,'$.canonical_inputs.receipts[0].fingerprint'),json_extract(handle_state_json,'$.canonical_inputs.pending[0].queue_position'),json_extract(handle_state_json,'$.canonical_inputs.pending[0].queued_at_ms') FROM worker_sessions WHERE id=?1")
        .bind(&s.carrier).bind(s.root.as_str()).bind(ATTEMPT).bind(LEGACY_OWNER_USER_ID).execute(&mut **tx).await
}

async fn retire(
    tx: &mut Transaction<'_, Sqlite>,
    s: &Seed,
) -> sqlx::Result<sqlx::sqlite::SqliteQueryResult> {
    sqlx::query("UPDATE conversation_submission_inputs SET effective=0, retirement_reason='human_resolution',
            retired_by_retry_intent_id=(SELECT retry_intent_id FROM conversation_input_retry_relations WHERE predecessor_submission_id=?1) WHERE submission_id=?1")
        .bind(s.root.as_str()).execute(&mut **tx).await
}

async fn reference(
    tx: &mut Transaction<'_, Sqlite>,
    s: &Seed,
    id: &str,
    turn: &str,
    root: &str,
    card: &str,
) -> sqlx::Result<sqlx::sqlite::SqliteQueryResult> {
    super::claim_storage::insert_turn_tx(
        tx,
        card,
        &s.carrier,
        &LogicalTurnId::parse(turn).unwrap(),
        &InputSubmissionId::parse(id).unwrap(),
        &provenance(),
    )
    .await
    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    sqlx::query(
        "INSERT INTO conversation_input_submissions(id, card_id, logical_turn_id, ordinal, kind,
            render_plan_json, render_plan_root_submission_id, render_plan_sha256, retry_intent_id, created_at_ms)
            VALUES(?1, ?2, ?3, 0, 'initial', NULL, ?4, ?5, (SELECT retry_intent_id FROM conversation_input_retry_relations WHERE predecessor_submission_id=?6), 1)",
    )
    .bind(id)
    .bind(card)
    .bind(turn)
    .bind(root)
    .bind(digest(&s.render).unwrap())
    .bind(s.root.as_str())
    .execute(&mut **tx)
    .await
}

async fn successor(tx: &mut Transaction<'_, Sqlite>, s: &Seed) {
    receipt(tx, s).await;
    relation(tx, s).await.unwrap();
    retire(tx, s).await.unwrap();
    reference(tx, s, SUCCESSOR, SUCCESSOR_TURN, s.root.as_str(), &s.card)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversation_submission_inputs(submission_id, input_position, input_kind, message_id,
            revision, observation_id, queue_position, queued_at_ms, envelope_id, system_addressable,
            effective) SELECT ?1, input_position, input_kind, message_id, revision, observation_id,
            queue_position, queued_at_ms, envelope_id, system_addressable, 1 FROM
            conversation_submission_inputs WHERE submission_id=?2")
        .bind(SUCCESSOR).bind(s.root.as_str()).execute(&mut **tx).await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_reference_reads_root_and_system_provenance() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    sqlx::query("SAVEPOINT inline_successor")
        .execute(&mut **tx)
        .await
        .unwrap();
    receipt(&mut tx, &s).await;
    relation(&mut tx, &s).await.unwrap();
    super::claim_storage::insert_turn_tx(
        &mut tx,
        &s.card,
        &s.carrier,
        &LogicalTurnId::parse(SUCCESSOR_TURN).unwrap(),
        &InputSubmissionId::parse(SUCCESSOR).unwrap(),
        &provenance(),
    )
    .await
    .unwrap();
    assert!(sqlx::query("INSERT INTO conversation_input_submissions(id, card_id, logical_turn_id, ordinal, kind,
            render_plan_json, render_plan_sha256, retry_intent_id, created_at_ms) VALUES(?1, ?2, ?3, 0, 'initial',
            ?4, ?5, (SELECT retry_intent_id FROM conversation_input_retry_relations LIMIT 1), 1)")
        .bind(SUCCESSOR).bind(&s.card).bind(SUCCESSOR_TURN).bind(serde_json::to_string(&s.render).unwrap()).bind(digest(&s.render).unwrap()).execute(&mut **tx).await.is_err(), "a reserved successor must never store a copied inline payload");
    sqlx::query("ROLLBACK TO inline_successor; RELEASE inline_successor")
        .execute(&mut **tx)
        .await
        .unwrap();
    successor(&mut tx, &s).await;
    let stored = submission_plan_tx(&mut tx, &InputSubmissionId::parse(SUCCESSOR).unwrap())
        .await
        .unwrap();
    assert_eq!(stored.plan, s.render);
    assert_eq!(stored.original_digest.as_str(), digest(&s.render).unwrap());
    let page = super::super::conversation_read::page_on_connection(&mut tx, &s.card, 0, 100, false)
        .await
        .unwrap();
    let display = serde_json::to_string(&page).unwrap();
    assert!(display.contains("frozen System"), "{display}");
    assert!(display.contains("frozen Human"), "{display}");
    let inline: Option<String> = sqlx::query_scalar(
        "SELECT render_plan_json FROM conversation_input_submissions WHERE id=?1",
    )
    .bind(SUCCESSOR)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert!(inline.is_none(), "no copied System payload");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_rejects_foreign_chain_and_mutable_reference() {
    let s = seed().await;
    let (_, other) = super::tests::fixture_card(&s.repo).await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    for (id, turn, root, card) in [
        (
            "11111111111111111111111111111111",
            "22222222222222222222222222222222",
            SUCCESSOR,
            s.card.as_str(),
        ),
        (
            "33333333333333333333333333333333",
            "44444444444444444444444444444444",
            s.root.as_str(),
            other.as_str(),
        ),
        (
            "55555555555555555555555555555555",
            "66666666666666666666666666666666",
            "55555555555555555555555555555555",
            s.card.as_str(),
        ),
    ] {
        assert!(reference(&mut tx, &s, id, turn, root, card).await.is_err());
    }
    for sql in [
        "UPDATE conversation_input_submissions SET render_plan_root_submission_id=NULL,render_plan_json='{}' WHERE id=?1",
        "UPDATE conversation_input_submissions SET render_plan_sha256=printf('%064d',0) WHERE id=?1",
        "UPDATE conversation_input_submissions SET render_plan_json='{}' WHERE id=?1",
        "INSERT OR REPLACE INTO conversation_input_submissions SELECT * FROM conversation_input_submissions WHERE id=?1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(SUCCESSOR)
                .execute(&mut **tx)
                .await
                .is_err(),
            "{sql}"
        );
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_root_erasure_propagates_to_all_references() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    let mut erased = s.render.clone();
    let SubmissionRenderPlan::V1 { segments } = &mut erased;
    for segment in segments {
        match segment {
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::System { payload, .. },
                ..
            } => {
                *payload = PrivateHistoryPayload::ErasedByPrivateHistoryOperation {
                    operation_id: super::tests::operation(),
                }
            }
            SubmissionRenderSegment::Context { payload } => {
                *payload = PrivateHistoryPayload::ErasedByPrivateHistoryOperation {
                    operation_id: super::tests::operation(),
                }
            }
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::Human { .. },
                ..
            } => {}
        }
    }
    sqlx::query("UPDATE conversation_input_submissions SET render_plan_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&erased).unwrap())
        .bind(s.root.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: super::tests::operation(),
                },
            )
            .unwrap(),
        )
        .bind(s.human.message_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    for id in [s.root.clone(), InputSubmissionId::parse(SUCCESSOR).unwrap()] {
        let stored = submission_plan_tx(&mut tx, &id).await.unwrap();
        assert_eq!(stored.plan, erased);
        assert_eq!(stored.original_digest.as_str(), digest(&s.render).unwrap());
    }
    assert!(matches!(
        revision_tx(&mut tx, &s.human).await.unwrap().payload,
        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
    ));
    let page = super::super::conversation_read::page_on_connection(&mut tx, &s.card, 0, 100, false)
        .await
        .unwrap();
    let display = serde_json::to_string(&page).unwrap();
    assert!(!display.contains("frozen System") && !display.contains("frozen Human"));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_retirement_requires_linked_receipt() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    assert!(retire(&mut tx, &s).await.is_err());
    assert!(relation(&mut tx, &s).await.is_err());
    receipt(&mut tx, &s).await;
    for path in [
        "$.decision.kind",
        "$.principal",
        "$.fingerprint",
        "$.decision.predecessor.attempt_id",
        "$.decision.predecessor.carrier_id",
        "$.decision.predecessor.submission_id",
        "$.decision.retry_intent_id",
        "$.key",
    ] {
        sqlx::query("SAVEPOINT invalid_receipt")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,
            '$.canonical_inputs.receipts[0]', json_set(json_extract(handle_state_json,
            '$.canonical_inputs.receipts[0]'), ?1, 'wrong')) WHERE id=?2",
        )
        .bind(path)
        .bind(&s.carrier)
        .execute(&mut **tx)
        .await
        .unwrap();
        assert!(relation(&mut tx, &s).await.is_err(), "{path}");
        sqlx::query("ROLLBACK TO invalid_receipt; RELEASE invalid_receipt")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    relation(&mut tx, &s).await.unwrap();
    let valid: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&s.carrier)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    // Retirement rechecks the authoritative receipt, not only a stale relation.
    sqlx::query(
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,
            '$.canonical_inputs.receipts', json('[]')) WHERE id=?1",
    )
    .bind(&s.carrier)
    .execute(&mut **tx)
    .await
    .unwrap();
    assert!(retire(&mut tx, &s).await.is_err());
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(valid)
        .bind(&s.carrier)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(retire(&mut tx, &s).await.unwrap().rows_affected(), 2);
    assert!(retire(&mut tx, &s).await.is_err());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_not_dispatched_and_human_retirement_are_distinct() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    for sql in [
        "UPDATE conversation_submission_inputs SET effective=0,retirement_reason='not_dispatched' WHERE submission_id=?1",
        "UPDATE conversation_submission_inputs SET effective=0,released_by_attempt_id=?2 WHERE submission_id=?1",
        "UPDATE conversation_submission_inputs SET effective=0 WHERE submission_id=?1",
        "UPDATE conversation_submission_inputs SET effective=0, retirement_reason='human_resolution',
            released_by_attempt_id=?2 WHERE submission_id=?1",
        "UPDATE conversation_submission_inputs SET retirement_reason='not_dispatched',
            released_by_attempt_id=?2 WHERE submission_id=?1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(s.root.as_str())
                .bind(ATTEMPT)
                .execute(&mut **tx)
                .await
                .is_err(),
            "{sql}"
        );
    }
    sqlx::query(
        "UPDATE conversation_submission_inputs SET effective=0, retirement_reason='not_dispatched',
            released_by_attempt_id=?1 WHERE submission_id=?2",
    )
    .bind(ATTEMPT)
    .bind(s.root.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    let rows = sqlx::query("SELECT retirement_reason,released_by_attempt_id,retired_by_retry_intent_id FROM conversation_submission_inputs").fetch_all(&mut **tx).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.get::<String, _>("retirement_reason"), "not_dispatched");
        assert_eq!(row.get::<String, _>("released_by_attempt_id"), ATTEMPT);
        assert!(
            row.get::<Option<String>, _>("retired_by_retry_intent_id")
                .is_none()
        );
    }
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    let row = sqlx::query(
        "SELECT retirement_reason, released_by_attempt_id, retired_by_retry_intent_id FROM
            conversation_submission_inputs WHERE submission_id=?1 LIMIT 1",
    )
    .bind(s.root.as_str())
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert_eq!(
        row.get::<String, _>("retirement_reason"),
        "human_resolution"
    );
    assert!(
        row.get::<Option<String>, _>("released_by_attempt_id")
            .is_none()
    );
    assert_eq!(
        row.get::<String, _>("retired_by_retry_intent_id"),
        sqlx::query_scalar::<_, String>(
            "SELECT retry_intent_id FROM conversation_input_retry_relations LIMIT 1"
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_storage_rollback_second_consumer_and_card_delete() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    assert!(sqlx::query("INSERT INTO conversation_submission_inputs SELECT submission_id, input_position+10, input_kind,
            message_id, revision, observation_id, queue_position, queued_at_ms, envelope_id,
            system_addressable, effective, released_by_attempt_id, retirement_reason,
            retired_by_retry_intent_id FROM conversation_submission_inputs WHERE
            submission_id=?1")
        .bind(s.root.as_str()).execute(&mut **tx).await.is_err(), "new claims must start effective and retire through the checked transition");
    assert!(relation(&mut tx, &s).await.is_err());
    assert!(sqlx::query("INSERT OR REPLACE INTO conversation_input_retry_relations SELECT * FROM conversation_input_retry_relations").execute(&mut **tx).await.is_err());
    assert!(
        sqlx::query("UPDATE conversation_input_retry_relations SET created_at_ms=2")
            .execute(&mut **tx)
            .await
            .is_err()
    );
    assert!(sqlx::query("INSERT OR REPLACE INTO conversation_submission_inputs SELECT ?1, input_position+10, input_kind,
            message_id, revision, observation_id, queue_position, queued_at_ms, envelope_id,
            system_addressable, effective, released_by_attempt_id, retirement_reason,
            retired_by_retry_intent_id FROM conversation_submission_inputs WHERE
            submission_id=?1").bind(SUCCESSOR).execute(&mut **tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_retry_relations")
            .fetch_one(s.repo.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(s.repo.pool())
        .await
        .unwrap(),
        2
    );
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    tx.commit().await.unwrap();
    assert!(
        sqlx::query("DELETE FROM conversation_input_submissions WHERE id=?1")
            .bind(s.root.as_str())
            .execute(s.repo.pool())
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM cards WHERE id=?1")
        .bind(&s.card)
        .execute(s.repo.pool())
        .await
        .unwrap();
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(s.repo.pool())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_retry_relations")
            .fetch_one(s.repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[path = "retry_storage_tests/upgrade.rs"]
mod upgrade;

#[tokio::test]
async fn canonical_retry_storage_upgrade_is_atomic_on_invalid_v4() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut originals = vec![];
    for state in ["failed", "superseded"] {
        let (_, worker, _) = super::held_projection_tests::held(&repo).await;
        let raw: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["schema_version"] = json!(4);
        let raw = value.to_string();
        sqlx::query("UPDATE worker_sessions SET state=?1, handle_state_json=?2 WHERE id=?3")
            .bind(state)
            .bind(&raw)
            .bind(&worker)
            .execute(repo.pool())
            .await
            .unwrap();
        originals.push((worker, raw));
    }
    // Production migration visits ID order: prove an earlier rewrite is rolled
    // back when a later retained carrier is invalid.
    originals.sort_by(|a, b| a.0.cmp(&b.0));
    let mut invalid: serde_json::Value = serde_json::from_str(&originals[1].1).unwrap();
    invalid["canonical_inputs"]["attempts"][0]
        .as_object_mut()
        .unwrap()
        .remove("invocation");
    originals[1].1 = invalid.to_string();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(&originals[1].1)
        .bind(&originals[1].0)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(
        super::super::canonical_input_revision_migration::migrate(repo.db())
            .await
            .is_err()
    );
    for (worker, raw) in &originals {
        let after: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(
            &after, raw,
            "invalid retained carrier must roll back every rewrite"
        );
    }
    let mut valid: serde_json::Value = serde_json::from_str(&originals[1].1).unwrap();
    valid["canonical_inputs"]["attempts"][0]["invocation"] =
        json!({"kind":"prior_invocation_not_recorded"});
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(valid.to_string())
        .bind(&originals[1].0)
        .execute(repo.pool())
        .await
        .unwrap();
    super::super::canonical_input_revision_migration::migrate(repo.db())
        .await
        .unwrap();
    for (worker, _) in originals {
        let raw: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["schema_version"],
            5
        );
    }
}

#[tokio::test]
async fn canonical_retry_intent_is_independent_of_crossing_submission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let relations: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('conversation_input_retry_relations')",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert!(relations.iter().any(|name| name == "retry_intent_id"));
    assert!(
        !relations
            .iter()
            .any(|name| name == "successor_submission_id" || name == "successor_logical_turn_id")
    );
    let submissions: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('conversation_input_submissions')")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert!(submissions.iter().any(|name| name == "retry_intent_id"));
}
