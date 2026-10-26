use super::super::{SqlxRepo, begin_immediate_tx, session_get_tx};
use super::tests::{body, fixture};
use super::transaction_tests::runtime;
use super::*;
use crate::planner_binding::PlannerBinding;
use calm_types::{harness::HarnessProfile, runtime::AgentProvider, worker::*};
use serde_json::json;
use sqlx::{Sqlite, Transaction};

pub(super) struct Protected;
#[async_trait::async_trait]
impl CarrierReplacementGuard for Protected {
    async fn recheck_tx(
        &self,
        _: &mut Transaction<'_, Sqlite>,
        _: &CurrentInputCarrier,
    ) -> crate::error::Result<()> {
        Ok(())
    }
}
fn binding() -> PlannerBinding {
    PlannerBinding {
        profile: HarnessProfile::Planner,
        provider: AgentProvider::Codex,
    }
}
pub(super) async fn expected<'a>(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    worker: &str,
    owner: &'a dyn CarrierReplacementGuard,
) -> ExpectedCarrier<'a> {
    let carrier = current_input_carrier_tx(tx, card, worker).await.unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    let shape = super::super::card_execution_shape_tx(tx, card)
        .await
        .unwrap();
    ExpectedCarrier {
        card_id: card.into(),
        track_id: track.clone(),
        worker_session_id: worker.into(),
        original_snapshot: carrier.original_snapshot().into(),
        execution: RuntimeExecutionFence::from_carrier(&carrier),
        binding: PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload).unwrap(),
        accepted_prefix: carrier.snapshot()["push_watermark"].as_i64().unwrap(),
        ingress: carrier_ingress_revision_tx(tx, card, &track, worker)
            .await
            .unwrap(),
        owner,
        revision: CallerRevision::Unversioned,
    }
}
async fn row(repo: &SqlxRepo, worker: &str) -> WorkerSession {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    session_get_tx(&mut tx, &WorkerSessionId(worker.into()))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn carrier_replacement_twice_in_one_transaction_and_rollback() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let first_expected = expected(&mut tx, &card, &worker, &Protected).await;
    let first = replace_idle_carrier_tx(
        &mut tx,
        first_expected,
        PlannerBinding {
            profile: HarnessProfile::Assistant,
            provider: AgentProvider::Claude,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.root_session_id.as_deref(), Some(worker.as_str()));
    let second_expected = expected(&mut tx, &card, first.new_worker.id.as_str(), &Protected).await;
    let second = replace_idle_carrier_tx(&mut tx, second_expected, binding())
        .await
        .unwrap();
    let current = current_input_carrier_tx(&mut tx, &card, second.new_worker.id.as_str())
        .await
        .unwrap();
    assert!(current.checkpoint().pending.is_empty());
    assert!(second.new_worker.thread_id.is_none());
    assert!(second.new_worker.mcp_token_hash.is_none());
    let root: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(&first.track_id)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(root, worker, "primitive never implicitly repoints a root");
    assert!(first.repoint_root_tx(&mut tx).await.is_err());
    second.repoint_root_tx(&mut tx).await.unwrap();
    assert!(
        second.repoint_root_tx(&mut tx).await.is_err(),
        "explicit root CAS refuses a changed root"
    );
    tx.rollback().await.unwrap();
    assert_eq!(row(&repo, &worker).await.state, WorkerSessionState::Idle);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn carrier_replacement_full_snapshot_prefix_binding_and_pointer_cas() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    for kind in [
        "snapshot",
        "prefix",
        "binding",
        "track",
        "pointer",
        "execution",
    ] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let mut stale = expected(&mut tx, &card, &worker, &Protected).await;
        match kind {
            "snapshot" => stale.original_snapshot.push(' '),
            "prefix" => stale.accepted_prefix += 1,
            "binding" => stale.binding.provider = AgentProvider::Claude,
            "track" => stale.track_id = "other".into(),
            "pointer" => {
                sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
                    .bind(&card)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "execution" => {
                let mut altered = current_input_carrier_tx(&mut tx, &card, &worker)
                    .await
                    .unwrap();
                altered.snapshot["last_seen_head"] = json!("changed");
                stale.execution = RuntimeExecutionFence::from_carrier(&altered);
            }
            _ => unreachable!(),
        }
        assert!(
            replace_idle_carrier_tx(&mut tx, stale, binding())
                .await
                .is_err(),
            "{kind}"
        );
        tx.rollback().await.unwrap();
        assert_eq!(row(&repo, &worker).await.state, WorkerSessionState::Idle);
    }
}

struct Revision(i64);
#[async_trait::async_trait]
impl CarrierReplacementGuard for Revision {
    async fn recheck_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> crate::error::Result<()> {
        let changed =
            sqlx::query("UPDATE cards SET updated_at=updated_at+1 WHERE id=?1 AND updated_at=?2")
                .bind(carrier.card_id())
                .bind(self.0)
                .execute(&mut **tx)
                .await?
                .rows_affected();
        if changed != 1 {
            return Err(crate::error::CalmError::Conflict("caller revision changed"));
        }
        Ok(())
    }
}
#[tokio::test]
async fn carrier_replacement_caller_revision_hook_runs_in_supplied_transaction() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT updated_at FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    let bad = Revision(revision - 1);
    let mut stale = expected(&mut tx, &card, &worker, &Protected).await;
    stale.revision = CallerRevision::Compare(&bad);
    assert!(
        replace_idle_carrier_tx(&mut tx, stale, binding())
            .await
            .is_err()
    );
    let good = Revision(revision);
    let mut current = expected(&mut tx, &card, &worker, &Protected).await;
    current.revision = CallerRevision::Compare(&good);
    replace_idle_carrier_tx(&mut tx, current, binding())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let original: i64 = sqlx::query_scalar("SELECT updated_at FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(original, revision);
}

#[tokio::test]
async fn carrier_replacement_revokes_both_stores_including_retired_mirrors() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    super::super::card_mcp_token_set_tx(&mut tx, &card, "card-hash")
        .await
        .unwrap();
    super::super::session_mcp_token_set_tx(&mut tx, &worker, "current-hash")
        .await
        .unwrap();
    let mut retired = session_get_tx(&mut tx, &WorkerSessionId(worker.clone()))
        .await
        .unwrap()
        .unwrap();
    retired.id = WorkerSessionId("stale-retired".into());
    retired.state = WorkerSessionState::Superseded;
    retired.mcp_token_hash = Some("stale-hash".into());
    super::super::session_insert_tx(&mut tx, retired)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    let rolled_back = replace_idle_carrier_tx(&mut tx, before, binding())
        .await
        .unwrap();
    rolled_back.repoint_root_tx(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        row(&repo, &worker).await.mcp_token_hash.as_deref(),
        Some("current-hash")
    );
    assert_eq!(
        row(&repo, "stale-retired").await.mcp_token_hash.as_deref(),
        Some("stale-hash")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    let changed = replace_idle_carrier_tx(&mut tx, before, binding())
        .await
        .unwrap();
    assert_eq!(changed.revoked_card_tokens, 1);
    assert_eq!(changed.revoked_session_mirrors, 2);
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1")
        .bind(&card)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    let mirrors: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM worker_sessions WHERE card_id=?1 AND mcp_token_hash IS NOT NULL",
    )
    .bind(&card)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert_eq!((tokens, mirrors), (0, 0));
    changed.repoint_root_tx(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        row(&repo, &worker).await.state,
        WorkerSessionState::Superseded
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        current_input_carrier_tx(&mut tx, &card, &worker)
            .await
            .is_err()
    );
    assert!(
        current_input_carrier_tx(&mut tx, &card, changed.new_worker.id.as_str())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn carrier_replacement_refuses_pending_active_malformed_and_unavailable() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    for kind in [
        "pending",
        "active_native",
        "starting",
        "failed",
        "rewind_phase",
        "compact_phase",
        "stop_phase",
        "unknown",
        "malformed",
        "historical",
    ] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let mut before = expected(&mut tx, &card, &worker, &Protected).await;
        match kind {
            "pending" => {
                let mut carrier = current_input_carrier_tx(&mut tx, &card, &worker)
                    .await
                    .unwrap();
                accept_pending_human_tx(
                    &mut tx,
                    &mut carrier,
                    calm_types::organization::LEGACY_OWNER_USER_ID,
                    body("pending"),
                )
                .await
                .unwrap();
            }
            "active_native" => {
                sqlx::query("UPDATE worker_sessions SET active_turn_id='native' WHERE id=?1")
                    .bind(&worker)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "starting" | "failed" => {
                sqlx::query("UPDATE worker_sessions SET state=?1 WHERE id=?2")
                    .bind(kind)
                    .bind(&worker)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            _ => {
                let mut snapshot: serde_json::Value =
                    serde_json::from_str(&before.original_snapshot).unwrap();
                match kind {
                    "rewind_phase" => {
                        snapshot["pending_rewind"] = serde_json::to_value(
                            calm_types::harness::snapshot::PersistedRewind::CodexV2 {
                                thread_id: "thread".into(),
                                before_turn_id: "turn".into(),
                                reverted: false,
                            },
                        )
                        .unwrap()
                    }
                    "compact_phase" => snapshot["phase"] = json!("compacting"),
                    "stop_phase" => snapshot["phase"] = json!("issuing_interrupt"),
                    "unknown" => snapshot["phase"] = json!("wedged"),
                    "malformed" => {
                        snapshot.as_object_mut().unwrap().remove("canonical_inputs");
                    }
                    "historical" => snapshot["schema_version"] = json!(1),
                    _ => unreachable!(),
                }
                sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
                    .bind(serde_json::to_string(&snapshot).unwrap())
                    .bind(&worker)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
        }
        if !matches!(kind, "malformed" | "historical" | "failed") {
            before = expected(&mut tx, &card, &worker, &Protected).await;
        }
        assert!(
            replace_idle_carrier_tx(&mut tx, before, binding())
                .await
                .is_err(),
            "{kind}"
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn carrier_replacement_ingress_revision_cas() {
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    for statement in [
        "INSERT INTO acp_managed_sessions(worker_session_id,registration_digest) VALUES(?1,'registration')",
        "UPDATE worker_sessions SET agent_session_id='new-native' WHERE id=?1",
        "UPDATE worker_sessions SET parent_session_id=id WHERE id=?1",
    ] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let before = expected(&mut tx, &card, &worker, &Protected).await;
        sqlx::query(statement)
            .bind(&worker)
            .execute(&mut **tx)
            .await
            .unwrap();
        assert!(
            replace_idle_carrier_tx(&mut tx, before, binding())
                .await
                .is_err(),
            "{statement}"
        );
        tx.rollback().await.unwrap();
    }
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    let mut proof = current_native_fact_tx(&mut tx, &card, &worker, false)
        .await
        .unwrap();
    super::super::transcript_projection::insert_transcript_row(
        &mut proof,
        "thread",
        None,
        None,
        None,
        "turn/completed",
        "{}",
        None,
    )
    .await
    .unwrap();
    assert!(
        replace_idle_carrier_tx(&mut tx, before, binding())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn carrier_replacement_refuses_unresolved_native_and_retry_work() {
    let s = super::retry_storage_tests::seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let before = expected(&mut tx, &s.card, &s.carrier, &Protected).await;
    assert!(
        replace_idle_carrier_tx(&mut tx, before, binding())
            .await
            .is_err()
    );
    super::retry::resolve_retry_tx(
        &mut tx,
        &s.card,
        calm_types::organization::LEGACY_OWNER_USER_ID,
        "replacement-refusal",
        &super::retry_storage_tests::request(&s),
        &super::transaction_tests::provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let before = expected(&mut tx, &s.card, &s.carrier, &Protected).await;
    assert!(
        replace_idle_carrier_tx(&mut tx, before, binding())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query(concat!("INSERT INTO ","acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_","at_ms) VALUES(?1,'client','turn','thread','native','{}','unknown',0)"))
        .bind(&worker).execute(&mut **tx).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    assert!(
        replace_idle_carrier_tx(&mut tx, before, binding())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn c4_carrier_replacement_refuses_retained_claim_then_allows_completion() {
    use super::super::provider_execution as owner;
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let owner::Cas::Applied(claim) = owner::admit(
        &mut tx,
        &worker,
        "c4-execution".into(),
        owner::Purpose::RewindCheck,
        owner::InitialRelease::Held,
    )
    .await
    .unwrap() else {
        panic!("admission lost")
    };
    super::super::session_fail_if_active_runtime_tx(&mut tx, &worker)
        .await
        .unwrap();
    let current = session_get_tx(&mut tx, &WorkerSessionId(worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        current.state.is_active_authority(),
        "R3 skips a terminal projection while its claim is retained"
    );
    assert!(
        super::super::session_mark_superseded_runtime_tx(&mut tx, &worker)
            .await
            .is_err()
    );
    assert!(
        super::super::session_complete_tx(&mut tx, &worker, WorkerSessionState::Exited)
            .await
            .is_err()
    );
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    assert!(matches!(
        replace_idle_carrier_tx(&mut tx, before, binding()).await,
        Err(crate::error::CalmError::Core(
            calm_types::error::CoreError::Conflict(_)
        ))
    ));
    assert!(matches!(
        owner::clear_on_completion(&mut tx, &worker, &claim)
            .await
            .unwrap(),
        owner::Cas::Applied(())
    ));
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    let replacement = replace_idle_carrier_tx(&mut tx, before, binding())
        .await
        .unwrap();
    assert_ne!(replacement.new_worker.id.as_str(), worker);
}

#[tokio::test]
async fn c4_admission_cannot_attach_to_a_replaced_carrier() {
    use super::super::provider_execution as owner;
    let (repo, _, card) = fixture().await;
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    replace_idle_carrier_tx(&mut tx, before, binding())
        .await
        .unwrap();
    // A transaction-local replacement may win after the request's last owner read,
    // before its admission transaction acquires the writer reservation.
    assert!(matches!(
        owner::admit(
            &mut tx,
            &worker,
            "late-execution".into(),
            owner::Purpose::RewindCheck,
            owner::InitialRelease::Held
        )
        .await
        .unwrap(),
        owner::Cas::LostOwnership
    ));
    assert!(owner::load(&mut **tx, &worker).await.unwrap().is_none());
}

#[tokio::test]
async fn u13_real_replacement_keeps_old_v1_and_boot_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("rotation.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (_, card) = super::tests::fixture_card(&repo).await;
    let historical = runtime(&repo, &card).await;
    let raw = r#"{"schema_version":1,"mode":"harness","pending_queue":[]}"#;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    super::super::session_set_handle_state_tx(
        &mut tx,
        &historical,
        Some(serde_json::from_str(raw).unwrap()),
    )
    .await
    .unwrap();
    super::super::session_supersede_active_tx(&mut tx, &historical, 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // The primitive only rotates a settled canonical current carrier. An older
    // historical row is already retired; no claim that V1 itself can rotate.
    let worker = runtime(&repo, &card).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = expected(&mut tx, &card, &worker, &Protected).await;
    let replacement = replace_idle_carrier_tx(&mut tx, before, binding())
        .await
        .unwrap();
    replacement.repoint_root_tx(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    let old = row(&repo, &historical).await;
    assert_eq!(old.state, WorkerSessionState::Superseded);
    assert_eq!(
        old.handle_state_json,
        Some(serde_json::from_str(raw).unwrap())
    );
    assert_eq!(
        row(&repo, &worker).await.state,
        WorkerSessionState::Superseded
    );
    repo.db().close().await;
    assert!(matches!(SqlxRepo::open(&url).await,
        Err(crate::error::CalmError::Internal(message)) if message.starts_with("conversation upgrade:")));
}
