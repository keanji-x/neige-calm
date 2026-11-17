//! Real database/session ownership and lifecycle through the held/ask owners.
use super::accounting_tests::{close, open};
use super::*;
use crate::db::RepoEventWrite;
use crate::event::{AskAnswer, Event, EventScope};
use crate::harness::run_loop::completed_commit_tests::Fixture;
use crate::ids::ActorId;
use crate::model::CardRole;
use calm_truth::decision_gate::{
    enforce_role_resolving_session, enforce_role_resolving_session_from_tx,
};

#[tokio::test]
async fn assistant_hold_live_binding_is_rechecked_by_answer_and_both_truth_substrates() {
    let fx = Fixture::for_role(CardRole::Assistant).await;
    let id = open(&fx.harness.inner, "live-binding").await;
    let source = fx
        .repo
        .events_for_track(fx.harness.inner.track_id.as_str(), &["ask.requested"], None)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let inner = fx.harness.inner.clone();
    let db = fx.repo.db().clone();
    db.writer().with_writer(move |writer| Box::pin(async move {
        let mut tx = writer.begin_immediate().await.unwrap();
        assert!(crate::ask::require_live_ask_tx(&mut tx, id).await.is_ok());
        for sql in [
            "UPDATE cards SET session_id=NULL WHERE id=?1",
            "UPDATE worker_sessions SET state='superseded' WHERE card_id=?1",
            "UPDATE worker_sessions SET provider='claude' WHERE card_id=?1",
            "UPDATE worker_sessions SET contract='executor' WHERE card_id=?1",
            "UPDATE cards SET payload=json_set(payload,'$.harness_profile','plain_chat') WHERE id=?1",
            "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id=?1",
        ] {
            sqlx::query("SAVEPOINT invalid_binding").execute(&mut **tx).await.unwrap();
            sqlx::query(sql).bind(inner.card_id.as_str()).execute(&mut **tx).await.unwrap();
            assert!(crate::ask::ask_answered_tx(&mut tx, &inner.track_id, id, vec![AskAnswer::Option(0)]).await.is_err(), "{sql}");
            for event in [&source.event, &Event::AskWithdrawn { ask_id: id, track_id: inner.track_id.clone() }] {
                let cached = enforce_role_resolving_session(&mut *tx, &source.actor, event, &source.scope, &inner.card_role_cache, &inner.track_area_cache).await;
                let live = enforce_role_resolving_session_from_tx(&mut *tx, &source.actor, event, &source.scope).await;
                assert_eq!(cached.as_ref().err().map(std::mem::discriminant), live.as_ref().err().map(std::mem::discriminant), "{sql}");
                assert!(live.is_err(), "{sql}");
            }
            sqlx::query("ROLLBACK TO invalid_binding").execute(&mut **tx).await.unwrap();
            sqlx::query("RELEASE invalid_binding").execute(&mut **tx).await.unwrap();
        }
        assert!(crate::ask::require_live_ask_tx(&mut tx, id).await.is_ok());
        tx.rollback().await.unwrap();
    })).await.unwrap();
    close(&fx.harness.inner, "live-binding").await;
}

#[tokio::test]
async fn assistant_hold_withdraw_and_carrier_recovery_are_scoped_to_the_native_owner() {
    let fx = Fixture::for_role(CardRole::Assistant).await;
    let id = open(&fx.harness.inner, "native-owner").await;
    let inner = fx.harness.inner.clone();
    let db = fx.repo.db().clone();
    db.writer().with_writer(move |writer| Box::pin(async move {
        let mut tx = writer.begin_immediate().await.unwrap();
        let source = calm_truth::db::sqlite::event_inputs::event_input_tx(&mut tx, id).await.unwrap().envelope;
        assert_eq!(source.actor, ActorId::AiManagedSession(session(&inner)));
        assert!(matches!(&source.scope, EventScope::Card { card, .. } if card == &inner.card_id));
        let projected: Vec<i64> = sqlx::query_scalar(crate::track_activity::sql::N1_OPEN_ASKS_SQL).bind(inner.track_id.as_str()).fetch_all(&mut **tx).await.unwrap();
        assert_eq!(projected.len(), 1, "the shared projection includes the actual Assistant owner");
        let withdraw = Event::AskWithdrawn { ask_id: id, track_id: inner.track_id.clone() };
        for (column, value) in [("actor", serde_json::to_string(&ActorId::AiManagedSession("other-session".into())).unwrap()), ("scope_card", "other-card".into())] {
            sqlx::query("SAVEPOINT wrong_source").execute(&mut **tx).await.unwrap();
            sqlx::query(&format!("UPDATE events SET {column}=?1 WHERE id=?2")).bind(value).bind(id).execute(&mut **tx).await.unwrap();
            assert!(enforce_role_resolving_session_from_tx(&mut *tx, &source.actor, &withdraw, &source.scope).await.is_ok());
            assert!(crate::ask::withdraw_hold_asks_tx(&mut tx, &inner.track_id, &session(&inner), &HashSet::new()).await.unwrap().is_empty());
            assert!(crate::ask::require_live_ask_tx(&mut tx, id).await.is_err());
            sqlx::query("ROLLBACK TO wrong_source").execute(&mut **tx).await.unwrap();
            sqlx::query("RELEASE wrong_source").execute(&mut **tx).await.unwrap();
        }
        assert!(crate::ask::require_no_open_carrier_asks_tx(&mut tx, inner.card_id.as_str()).await.is_err());
        // A sibling carrier on the same track is not blocked by this native Hold.
        let sibling = crate::db::sqlite::card_create_with_id_tx(&mut tx, "sibling".into(), crate::model::NewCard {
            track_id: inner.track_id.clone(), title: None, kind: "codex".into(), sort: None,
            payload: serde_json::json!({"harness_profile":"assistant", "assistant_provider":"codex","permission_mode":"never"}),
        }, CardRole::Assistant, false, &inner.card_role_cache).await.unwrap();
        assert!(crate::ask::require_no_open_carrier_asks_tx(&mut tx, sibling.id.as_str()).await.is_ok());
        tx.rollback().await.unwrap();
    })).await.unwrap();
    close(&fx.harness.inner, "native-owner").await;
    assert!(
        fx.repo
            .events_for_track(fx.harness.inner.track_id.as_str(), &["ask.withdrawn"], None)
            .await
            .unwrap()
            .iter()
            .any(|r| matches!(r.event, Event::AskWithdrawn { ask_id, .. } if ask_id == id))
    );
}

#[tokio::test]
async fn planner_hold_historical_track_scope_keeps_answer_and_withdraw_ownership() {
    let fx = Fixture::new().await;
    let id = open(&fx.harness.inner, "historical").await;
    let inner = fx.harness.inner.clone();
    let db = fx.repo.db().clone();
    db.writer()
        .with_writer(move |writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                // Historical representation only; no new writer produces Track-scope Hold.
                sqlx::query("UPDATE events SET scope_kind='track',scope_card=NULL WHERE id=?1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                assert!(
                    crate::ask::ask_answered_tx(
                        &mut tx,
                        &inner.track_id,
                        id,
                        vec![AskAnswer::Option(1)]
                    )
                    .await
                    .is_ok()
                );
                let events = crate::ask::withdraw_hold_asks_tx(
                    &mut tx,
                    &inner.track_id,
                    &session(&inner),
                    &HashSet::new(),
                )
                .await
                .unwrap();
                assert_eq!(events.len(), 1);
                let (actor, scope, event) = &events[0];
                assert!(
                    enforce_role_resolving_session_from_tx(&mut *tx, actor, event, scope)
                        .await
                        .is_ok()
                );
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
    close(&fx.harness.inner, "historical").await;
}

#[tokio::test]
async fn assistant_hold_orphan_sweep_and_turn_end_close_once_and_never_answer_again() {
    let fx = Fixture::for_role(CardRole::Assistant).await;
    let inner = &fx.harness.inner;
    let orphan = open(inner, "orphan").await;
    drop(inner.held_requests.take(orphan));
    sweep(inner).await;
    sweep(inner).await;
    let active = open(inner, "turn-end").await;
    end_turn(inner).await;
    end_turn(inner).await;
    let withdrawn = fx
        .repo
        .events_for_track(inner.track_id.as_str(), &["ask.withdrawn"], None)
        .await
        .unwrap();
    for id in [orphan, active] {
        assert_eq!(
            withdrawn
                .iter()
                .filter(|r| matches!(r.event, Event::AskWithdrawn { ask_id, .. } if ask_id == id))
                .count(),
            1
        );
    }
    assert!(inner.held_requests.ask_ids().is_empty());
}

#[tokio::test]
async fn plain_chat_hold_open_has_no_authority_or_watchdog_credit() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl HeldResponder for Dropped {
        fn respond(self: Box<Self>, _: usize) {
            panic!("PlainChat cannot acquire a native reply");
        }
    }
    let fx = Fixture::for_role(CardRole::Worker).await;
    let inner = &fx.harness.inner;
    let dropped = Arc::new(AtomicBool::new(false));
    on_message(
        inner,
        HeldRequestMessage::Open {
            request_key: RequestKey("plain-chat".into()),
            connection: ConnectionId("plain-chat".into()),
            questions: vec![AskQuestion {
                detail: None,
                title: "Run?".into(),
                options: vec!["Allow".into(), "Deny".into()],
            }],
            responder: Box::new(Dropped(dropped.clone())),
        },
    )
    .await;
    assert!(dropped.load(Ordering::SeqCst));
    assert!(inner.held_requests.ask_ids().is_empty());
    assert_eq!(inner.held_requests.time().total, std::time::Duration::ZERO);
    assert!(
        fx.repo
            .events_for_track(inner.track_id.as_str(), &["ask.requested"], None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn assistant_hold_failed_owner_drops_then_recovery_sweeps_without_replay() {
    let fx = Fixture::for_role(CardRole::Assistant).await;
    let inner = &fx.harness.inner;
    let id = open(inner, "failed-owner").await;
    let db = fx.repo.db().clone();
    db.writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
                .bind(inner.worker_session_id.clone()),
        )
        .await
        .unwrap();
    close(inner, "failed-owner").await;
    assert!(inner.held_requests.ask_ids().is_empty());
    assert!(
        fx.repo
            .events_for_track(inner.track_id.as_str(), &["ask.withdrawn"], None)
            .await
            .unwrap()
            .is_empty(),
        "an inactive session cannot borrow Kernel cleanup authority"
    );
    let owner = inner.clone();
    db.writer()
        .with_writer(move |writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                assert!(
                    crate::ask::ask_answered_tx(
                        &mut tx,
                        &owner.track_id,
                        id,
                        vec![AskAnswer::Option(0)]
                    )
                    .await
                    .is_err()
                );
                assert!(
                    crate::ask::require_no_open_carrier_asks_tx(&mut tx, owner.card_id.as_str())
                        .await
                        .is_ok()
                );
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1")
                .bind(inner.worker_session_id.clone()),
        )
        .await
        .unwrap();
    sweep(inner).await;
    sweep(inner).await;
    let closed = fx
        .repo
        .events_for_track(inner.track_id.as_str(), &["ask.withdrawn"], None)
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert!(matches!(closed[0].event, Event::AskWithdrawn { ask_id, .. } if ask_id == id));
    assert!(
        fx.repo
            .events_for_track(inner.track_id.as_str(), &["ask.answered"], None)
            .await
            .unwrap()
            .is_empty()
    );
}
