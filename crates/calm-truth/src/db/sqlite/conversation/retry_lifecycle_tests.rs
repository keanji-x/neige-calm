//! Committed Retry facts follow canonical card lifetime, not runtime lifetime.
use super::super::{
    area_delete_tx, begin_immediate_tx, card_delete_tx, session_delete_tx, track_delete_tx,
};
use super::retry_storage_tests::{Seed, request, seed};
use super::transaction_tests::provenance;
use super::*;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;

async fn committed_retry() -> (Seed, InputSubmissionId) {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let decision = super::retry::resolve_retry_tx(
        &mut tx,
        &s.card,
        LEGACY_OWNER_USER_ID,
        "delete-lifecycle",
        &request(&s),
        &provenance(),
    )
    .await
    .unwrap()
    .unwrap();
    let HumanInputResolutionDecision::Retry {
        retry_intent_id, ..
    } = decision
    else {
        panic!("expected Retry");
    };
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let successor = claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        InitialClaim::ExplicitSuccessor { retry_intent_id },
        &provenance(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (s, successor.submission_id)
}

#[tokio::test]
async fn canonical_retry_survives_session_owner_delete() {
    let (s, successor) = committed_retry().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    session_delete_tx(&mut tx, &s.carrier).await.unwrap();
    tx.commit()
        .await
        .expect("runtime deletion must not depend on retained retry facts");
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    assert_eq!(
        submission_plan_tx(&mut tx, &s.root).await.unwrap().plan,
        s.render
    );
    assert_eq!(
        submission_plan_tx(&mut tx, &successor).await.unwrap().plan,
        s.render
    );
    let retained: (String, i64) = sqlx::query_as(
        "SELECT carrier_id, (SELECT count(*) FROM worker_sessions WHERE id=carrier_id) FROM conversation_input_retry_relations",
    ).fetch_one(&mut **tx).await.unwrap();
    assert_eq!(retained, (s.carrier, 0));
    let page = super::super::conversation_read::page_on_connection(&mut tx, &s.card, 0, 100, false)
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&page)
            .unwrap()
            .contains("frozen Human")
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut **tx)
            .await
            .unwrap()
            .is_empty()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_card_owner_delete_cascades() {
    assert_owner_delete("card").await;
}

#[tokio::test]
async fn canonical_retry_track_owner_delete_cascades() {
    assert_owner_delete("track").await;
}

#[tokio::test]
async fn canonical_retry_area_owner_delete_cascades() {
    assert_owner_delete("area").await;
}

async fn assert_owner_delete(owner: &str) {
    let (s, successor) = committed_retry().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let (track, area): (String, String) = sqlx::query_as(
        "SELECT c.track_id,t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1",
    )
    .bind(&s.card)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    match owner {
        "card" => card_delete_tx(&mut tx, &s.card, s.repo.card_role_cache())
            .await
            .unwrap(),
        "track" => track_delete_tx(&mut tx, &track, s.repo.track_area_cache())
            .await
            .unwrap(),
        "area" => area_delete_tx(&mut tx, &area).await.unwrap(),
        _ => unreachable!(),
    }
    tx.commit()
        .await
        .expect("owner deletion must cascade all canonical retry facts");
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    for table in [
        "conversation_input_retry_relations",
        "conversation_turns",
        "conversation_input_submissions",
        "conversation_submission_inputs",
        "conversation_presentations",
        "conversation_messages",
        "worker_sessions",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut **tx)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} must follow whole-card deletion");
    }
    assert!(submission_plan_tx(&mut tx, &successor).await.is_err());
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut **tx)
            .await
            .unwrap()
            .is_empty()
    );
    tx.commit().await.unwrap();
}
