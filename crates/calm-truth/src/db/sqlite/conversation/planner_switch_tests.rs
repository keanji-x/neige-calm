use super::super::{self as db, begin_immediate_tx, track_planner::*};
use super::carrier_replacement_tests::Protected;
use super::tests::{body, fixture};
use super::transaction_tests::runtime;
use super::*;
use crate::model::{CardRole, NewCard};
use calm_types::{
    harness::snapshot::CanonicalHarnessSnapshot, organization::LEGACY_OWNER_USER_ID,
    runtime::AgentProvider, worker::*,
};
use serde_json::json;
use sqlx::{Sqlite, Transaction};

struct Pair {
    repo: db::SqlxRepo,
    track: String,
    a: String,
    b: String,
}
async fn pair() -> Pair {
    let (repo, _, a) = fixture().await;
    runtime(&repo, &a).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&a)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    let b = db::card_create_with_id_tx(&mut tx, crate::model::new_id(), NewCard {
        track_id: track.clone().into(), kind: "codex".into(), sort: None,
        payload: json!({"assistant_provider":"codex","harness_profile":"assistant","custom":"kept"}), title: Some("Assistant history".into()),
    }, CardRole::Assistant, true, repo.card_role_cache()).await.unwrap().id.to_string();
    db::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::managed_assistant(
            crate::model::new_id(),
            b.clone(),
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            None,
            serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap(),
            0,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Pair { repo, track, a, b }
}
async fn expected<'a>(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    owner: &'a dyn CarrierReplacementGuard,
) -> ExpectedCarrier<'a> {
    let worker: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    super::carrier_replacement_tests::expected(tx, card, &worker, owner).await
}
fn request<'a>(p: &'a Pair, reverse: bool, revision: i64) -> ExpectedPlanner<'a> {
    ExpectedPlanner {
        track_id: &p.track,
        planner_card_id: if reverse { &p.b } else { &p.a },
        target_card_id: if reverse { &p.a } else { &p.b },
        revision: PlannerRevision(revision),
        user_id: LEGACY_OWNER_USER_ID,
    }
}
async fn switch(
    p: &Pair,
    reverse: bool,
    revision: i64,
) -> Result<PlannerSwitchReceipt, PlannerSwitchError> {
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    let req = request(p, reverse, revision);
    let a = expected(&mut tx, req.planner_card_id, &Protected).await;
    let b = expected(&mut tx, req.target_card_id, &Protected).await;
    match switch_planner_tx(&mut tx, req, (a, b)).await {
        Ok(receipt) => {
            tx.commit().await.unwrap();
            Ok(receipt)
        }
        Err(error) => {
            tx.rollback().await.unwrap();
            Err(error)
        }
    }
}
async fn tenure(p: &Pair) -> (i64, i64, i64) {
    sqlx::query_as(concat!("SELECT planner_revision,planner_event_baseline,(SELECT count(*) FROM track_planner_changes WHERE ","track_id=tracks.id) FROM tracks WHERE id=?1"))
        .bind(&p.track).fetch_one(p.repo.pool()).await.unwrap()
}

#[tokio::test]
async fn planner_switch_pair_commit_root_history() {
    let p = pair().await;
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    // A pre-existing, owner-qualified event tail: baseline must use the real id.
    let track = db::track_get_tx(&mut tx, &p.track.clone().into())
        .await
        .unwrap();
    let scope = crate::event::EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let tail = db::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::User,
        &scope,
        None,
        &crate::event::Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(track, None)),
    )
    .await
    .unwrap();
    assert!(tail > 0);
    for (card, watermark) in [(&p.a, 3), (&p.b, 7)] {
        sqlx::query(concat!("UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.push_watermark',?1),","parent_session_id=id,requester_session_id=id,mcp_token_hash='secret-'||id WHERE card_id=?2"))
            .bind(watermark).bind(card).execute(&mut **tx).await.unwrap();
        db::card_mcp_token_set_tx(&mut tx, card, card)
            .await
            .unwrap();
    }
    for card in [&p.a, &p.b] {
        let e = expected(&mut tx, card, &Protected).await;
        let mut proof = current_native_fact_tx(&mut tx, card, &e.worker_session_id, false)
            .await
            .unwrap();
        let row_id = db::transcript_projection::insert_transcript_row(
            &mut proof,
            "thread",
            None,
            Some("reply"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
        allocate_presentation_tx(&mut tx, card, PresentationSource::NativeItem { row_id })
            .await
            .unwrap();
    }
    let a = expected(&mut tx, &p.a, &Protected).await;
    let b = expected(&mut tx, &p.b, &Protected).await;
    let before: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id,handle_state_json,contract FROM worker_sessions ORDER BY id")
            .fetch_all(&mut **tx)
            .await
            .unwrap();
    let receipt = switch_planner_tx(&mut tx, request(&p, false, 0), (a, b))
        .await
        .unwrap();
    for (card, planner) in [(&p.a, false), (&p.b, true)] {
        let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(card)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            payload.get("planner_harness"),
            planner.then_some(&json!(true))
        );
        assert_eq!(
            payload[if planner {
                "planner_provider"
            } else {
                "assistant_provider"
            }],
            "codex"
        );
    }
    assert_eq!(receipt.event_baseline, tail);
    assert_eq!(
        (receipt.demoted.watermark, receipt.promoted.watermark),
        (3, 7)
    );
    assert_eq!(receipt.events.len(), 2);
    assert!(receipt.demoted.history_boundary > 0 && receipt.promoted.history_boundary > 0);
    db::append_decision_events_in_tx(
        &mut tx,
        &crate::ids::ActorId::User,
        &scope,
        None,
        &receipt.events,
    )
    .await
    .unwrap();
    let root: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(&p.track)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(root, receipt.promoted.new_worker.id.as_str());
    for replacement in [&receipt.demoted, &receipt.promoted] {
        let old = db::session_get_tx(&mut tx, &replacement.old_worker.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.state, WorkerSessionState::Superseded);
        assert_eq!(old.parent_session_id, Some(old.id.clone()));
        assert_eq!(old.requester_session_id, Some(old.id.clone()));
        assert!(old.mcp_token_hash.is_none());
        let original = before.iter().find(|r| r.0 == old.id.as_str()).unwrap();
        assert_eq!(old.contract.as_db_str(), original.2);
        assert_eq!(
            serde_json::to_string(&old.handle_state_json.unwrap()).unwrap(),
            original.1
        );
        assert!(replacement.new_worker.handle_state_json.as_ref().unwrap()["canonical_inputs"]["pending"].as_array().unwrap().is_empty());
    }
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens")
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(tokens, 0);
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_presentations")
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(history, 2);
    let preference: String =
        sqlx::query_scalar("SELECT json_extract(payload,'$.custom') FROM cards WHERE id=?1")
            .bind(&p.b)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    assert_eq!(preference, "kept");
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id=?1")
        .bind(&p.b)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("Assistant history"));
    let fact: (String,String,i64,i64,i64) = sqlx::query_as("SELECT from_card_id,to_card_id,from_watermark,to_watermark,event_baseline FROM track_planner_changes").fetch_one(&mut **tx).await.unwrap();
    assert_eq!(fact, (p.a.clone(), p.b.clone(), 3, 7, tail));
    tx.commit().await.unwrap();
    assert_eq!(tenure(&p).await, (1, tail, 1));
}

struct FailSecond(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl CarrierReplacementGuard for FailSecond {
    async fn recheck_tx(
        &self,
        _: &mut Transaction<'_, Sqlite>,
        _: &CurrentInputCarrier,
    ) -> crate::error::Result<()> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            return Err(crate::error::TruthError::CarrierReplacementRefused(
                ReplacementRefusal::SessionUnavailable,
            ));
        }
        Ok(())
    }
}
#[tokio::test]
async fn planner_switch_second_replace_failure_rolls_back() {
    let p = pair().await;
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    db::card_mcp_token_set_tx(&mut tx, &p.a, "retained")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    let a = expected(&mut tx, &p.a, &Protected).await;
    let fail = FailSecond(std::sync::atomic::AtomicUsize::new(0));
    let b = expected(&mut tx, &p.b, &fail).await;
    let worker = a.worker_session_id.clone();
    let before_a = carrier_ingress_revision_tx(&mut tx, &p.a, &p.track, &a.worker_session_id)
        .await
        .unwrap();
    let before_b = carrier_ingress_revision_tx(&mut tx, &p.b, &p.track, &b.worker_session_id)
        .await
        .unwrap();
    let target_worker = b.worker_session_id.clone();
    let error = switch_planner_tx(&mut tx, request(&p, false, 0), (a, b))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code(), Some("planner_session_unavailable"));
    let role: String = sqlx::query_scalar("SELECT role FROM cards WHERE id=?1")
        .bind(&p.a)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        role, "assistant",
        "second replace failed after the first wrote"
    );
    tx.rollback().await.unwrap();
    assert_eq!(tenure(&p).await, (0, 0, 0));
    let current: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(&p.a)
        .fetch_one(p.repo.pool())
        .await
        .unwrap();
    assert_eq!(current, worker);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions")
        .fetch_one(p.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    assert_eq!(
        carrier_ingress_revision_tx(&mut tx, &p.a, &p.track, &worker)
            .await
            .unwrap(),
        before_a
    );
    assert_eq!(
        carrier_ingress_revision_tx(&mut tx, &p.b, &p.track, &target_worker)
            .await
            .unwrap(),
        before_b
    );
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1")
        .bind(&p.a)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(tokens, 1);
}

#[tokio::test]
async fn planner_switch_revision_aba() {
    let p = pair().await;
    switch(&p, false, 0).await.unwrap();
    switch(&p, true, 1).await.unwrap();
    assert_eq!(tenure(&p).await, (2, 0, 2));
    assert_eq!(
        switch(&p, false, 0).await.err().unwrap().code(),
        Some("planner_changed")
    );
    assert_eq!(tenure(&p).await, (2, 0, 2));
    sqlx::query("UPDATE tracks SET planner_revision=?1 WHERE id=?2")
        .bind(i64::MAX)
        .bind(&p.track)
        .execute(p.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        switch(&p, false, i64::MAX).await.err().unwrap().code(),
        Some("planner_changed")
    );
}

#[tokio::test]
async fn planner_switch_compare_full_ingress() {
    let p = pair().await;
    for card in [&p.a, &p.b] {
        let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
        let a = expected(&mut tx, &p.a, &Protected).await;
        let b = expected(&mut tx, &p.b, &Protected).await;
        sqlx::query(
            "UPDATE worker_sessions SET agent_session_id='same-sequence-edit' WHERE card_id=?1",
        )
        .bind(card)
        .execute(&mut **tx)
        .await
        .unwrap();
        assert_eq!(
            switch_planner_tx(&mut tx, request(&p, false, 0), (a, b))
                .await
                .err()
                .unwrap()
                .code(),
            Some("planner_changed")
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn planner_switch_typed_refusals() {
    let p = pair().await;
    for card in [&p.a, &p.b] {
        for (kind, code) in [
            ("pending", "planner_pending_messages"),
            ("submission", "planner_pending_submission"),
            ("active", "planner_turn_in_flight"),
            ("historical", "planner_historical_storage"),
            ("malformed", "storage_cause"),
            ("unavailable", "planner_session_unavailable"),
            ("binding", "planner_binding_unsupported"),
            ("target", "planner_target_invalid"),
        ] {
            let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
            let mut a = expected(&mut tx, &p.a, &Protected).await;
            let mut b = expected(&mut tx, &p.b, &Protected).await;
            let worker = if card == &p.a {
                &a.worker_session_id
            } else {
                &b.worker_session_id
            }
            .clone();
            match kind {
                "pending" => {
                    let mut carrier = current_input_carrier_tx(&mut tx, card, &worker)
                        .await
                        .unwrap();
                    accept_pending_human_tx(
                        &mut tx,
                        &mut carrier,
                        LEGACY_OWNER_USER_ID,
                        body("busy"),
                    )
                    .await
                    .unwrap();
                }
                "submission" => {
                    sqlx::query(concat!("INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,","input_json,state,created_at_ms) VALUES(?1,'client','turn','thread','native','{}','unknown',0)")).bind(&worker).execute(&mut **tx).await.unwrap();
                }
                "active" => {
                    sqlx::query("UPDATE worker_sessions SET active_turn_id='turn' WHERE id=?1")
                        .bind(&worker)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                "malformed" => {
                    sqlx::query("UPDATE worker_sessions SET handle_state_json=json_remove(handle_state_json,'$.canonical_inputs') WHERE id=?1")
                        .bind(&worker).execute(&mut **tx).await.unwrap();
                }
                "historical" => {
                    sqlx::query("UPDATE worker_sessions SET handle_state_json='{}' WHERE id=?1")
                        .bind(&worker)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                "unavailable" => {
                    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
                        .bind(&worker)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                "binding" => {
                    sqlx::query("UPDATE cards SET payload='{}' WHERE id=?1")
                        .bind(card)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                "target" => {
                    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                        .bind(&p.track)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            if matches!(kind, "pending" | "submission" | "active") {
                let observed = expected(&mut tx, card, &Protected).await;
                if card == &p.a {
                    a = observed;
                } else {
                    b = observed;
                }
            }
            assert_eq!(
                switch_planner_tx(&mut tx, request(&p, false, 0), (a, b))
                    .await
                    .err()
                    .unwrap()
                    .code(),
                if code == "storage_cause" {
                    None
                } else {
                    Some(code)
                },
                "{card} {kind}"
            );
            tx.rollback().await.unwrap();
            assert_eq!(tenure(&p).await, (0, 0, 0));
        }
    }
}

// Compare every persisted column, including both roles/bindings, worker pointers,
// old/new workers and credentials, root, tenure, switching facts and event bytes.
// Schema discovery keeps new columns covered without a second field inventory.
async fn switch_state(tx: &mut Transaction<'_, Sqlite>) -> Vec<(&'static str, Vec<String>)> {
    let mut state = Vec::new();
    for table in [
        "cards",
        "worker_sessions",
        "card_mcp_tokens",
        "tracks",
        "track_planner_changes",
        "events",
        "assistant_conversation_archives",
    ] {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(table)
                .fetch_all(&mut **tx)
                .await
                .unwrap();
        assert!(!columns.is_empty(), "missing table {table}");
        let columns = columns
            .iter()
            .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        let rows = sqlx::query_scalar(&format!(
            "SELECT json_array({columns}) FROM {table} ORDER BY rowid"
        ))
        .fetch_all(&mut **tx)
        .await
        .unwrap();
        state.push((table, rows));
    }
    state
}

async fn rollback_pair() -> Pair {
    let p = pair().await;
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    for card in [&p.a, &p.b] {
        db::card_mcp_token_set_tx(&mut tx, card, &format!("card-secret-{card}"))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE worker_sessions SET mcp_token_hash='worker-secret-'||id WHERE card_id=?1",
        )
        .bind(card)
        .execute(&mut **tx)
        .await
        .unwrap();
    }
    let track = db::track_get_tx(&mut tx, &p.track.clone().into())
        .await
        .unwrap();
    db::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::User,
        &crate::event::EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        },
        None,
        &crate::event::Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(track, None)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    p
}

#[tokio::test]
async fn planner_switch_late_sql_failure_rolls_back_full_state() {
    let p = rollback_pair().await;
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    let before = switch_state(&mut tx).await;
    let a = expected(&mut tx, &p.a, &Protected).await;
    let b = expected(&mut tx, &p.b, &Protected).await;
    let req = ExpectedPlanner {
        user_id: "nonexistent-switch-user",
        ..request(&p, false, 0)
    };
    let error = switch_planner_tx(&mut tx, req, (a, b)).await.err().unwrap();
    assert!(
        matches!(&error, PlannerSwitchError::Storage(_)),
        "{error:?}"
    );
    assert_eq!(error.code(), None);
    assert!(
        matches!(
            &error,
            PlannerSwitchError::Storage(crate::error::TruthError::Db(sqlx::Error::Database(cause)))
                if cause.is_foreign_key_violation()
        ),
        "{error:?}"
    );
    // SQLite FK failure is statement-local: prove we reached the fact INSERT
    // after both replacements, root repoint and tenure/baseline writes.
    let progress: (String, String, bool, i64, i64) = sqlx::query_as(
        "SELECT a.role,b.role,t.root_session_id=targ.session_id,t.planner_revision,t.planner_event_baseline
         FROM tracks t JOIN cards a ON a.id=?2 JOIN cards b ON b.id=?3
         JOIN cards targ ON targ.id=?3 WHERE t.id=?1")
        .bind(&p.track).bind(&p.a).bind(&p.b).fetch_one(&mut **tx).await.unwrap();
    assert_eq!(progress.0, "assistant");
    assert_eq!(progress.1, "planner");
    assert!(progress.2);
    assert_eq!(progress.3, 1);
    assert!(progress.4 > 0);
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    assert_eq!(switch_state(&mut tx).await, before);
}

#[tokio::test]
async fn planner_switch_revision_overflow_rolls_back_full_state() {
    let p = rollback_pair().await;
    sqlx::query("UPDATE tracks SET planner_revision=?1 WHERE id=?2")
        .bind(i64::MAX)
        .bind(&p.track)
        .execute(p.repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    let before = switch_state(&mut tx).await;
    let a = expected(&mut tx, &p.a, &Protected).await;
    let b = expected(&mut tx, &p.b, &Protected).await;
    let error = switch_planner_tx(&mut tx, request(&p, false, i64::MAX), (a, b))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code(), Some("planner_changed"));
    let role: String = sqlx::query_scalar("SELECT role FROM cards WHERE id=?1")
        .bind(&p.b)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(role, "planner", "overflow occurs after replacements");
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
    assert_eq!(switch_state(&mut tx).await, before);
}

#[tokio::test]
async fn planner_switch_invalid_targets_leave_full_state_unchanged() {
    for case in [
        "same",
        "cross-track",
        "archived",
        "worker",
        "kind",
        "closed",
    ] {
        let p = rollback_pair().await;
        let (_, other) = super::tests::fixture_card(&p.repo).await;
        let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
        let original = switch_state(&mut tx).await;
        // Observe valid carriers first; malformed target metadata must be refused
        // by the switch's own admission, before carrier replacement can run.
        let a = expected(&mut tx, &p.a, &Protected).await;
        let mut b = expected(
            &mut tx,
            if case == "same" { &p.a } else { &p.b },
            &Protected,
        )
        .await;
        let mut req = request(&p, false, 0);
        match case {
            "same" => req.target_card_id = &p.a,
            "cross-track" => {
                let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                    .bind(&other)
                    .fetch_one(&mut **tx)
                    .await
                    .unwrap();
                sqlx::query("UPDATE cards SET track_id=?1 WHERE id=?2")
                    .bind(&track)
                    .bind(&p.b)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                b.track_id = track;
            }
            "archived" => {
                db::conversation_archive_set_tx(&mut tx, &p.b, true)
                    .await
                    .unwrap();
            }
            "worker" => {
                sqlx::query("UPDATE cards SET role='worker' WHERE id=?1")
                    .bind(&p.b)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "kind" => {
                sqlx::query("UPDATE cards SET kind='note' WHERE id=?1")
                    .bind(&p.b)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "closed" => {
                sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                    .bind(&p.track)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = switch_state(&mut tx).await;
        let error = switch_planner_tx(&mut tx, req, (a, b)).await.err().unwrap();
        assert_eq!(
            error.code(),
            Some("planner_target_invalid"),
            "{case}: {error:?}"
        );
        assert_eq!(
            switch_state(&mut tx).await,
            before,
            "{case}: admission wrote state"
        );
        tx.rollback().await.unwrap();
        let mut tx = begin_immediate_tx(p.repo.db()).await.unwrap();
        assert_eq!(
            switch_state(&mut tx).await,
            original,
            "{case}: rollback changed state"
        );
    }
}
