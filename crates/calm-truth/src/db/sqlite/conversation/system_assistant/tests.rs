//! Real schema, original event and the sole production acceptance writer.
use super::*;
use crate::db::sqlite::{self as db, conversation::*};
use calm_types::{event::EventScope, harness::snapshot::CanonicalHarnessSnapshot};

const SOURCE: SystemAssistantSource = SystemAssistantSource::TodaySummary;

struct Fixture {
    repo: db::SqlxRepo,
    area: String,
    card: String,
    track: String,
    worker: String,
}
impl Fixture {
    async fn new() -> Self {
        let (repo, area, card) = super::super::tests::fixture().await;
        let mut tx = db::begin_immediate_tx(repo.db()).await.unwrap();
        sqlx::query("UPDATE cards SET role='assistant',payload='{\"harness_profile\":\"assistant\",\"assistant_provider\":\"codex\"}' WHERE id=?1")
            .bind(&card).execute(&mut **tx).await.unwrap();
        let worker = crate::model::new_id();
        Self::start_worker(&mut tx, &card, &worker).await;
        let track = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
            .bind(&card)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        Self {
            repo,
            area,
            card,
            track,
            worker,
        }
    }
    async fn start_worker(tx: &mut Transaction<'_, Sqlite>, card: &str, worker: &str) {
        db::session_start_runtime_tx(
            tx,
            crate::session_projection_repo::WorkerSessionInit::managed_assistant(
                worker.into(),
                card.into(),
                calm_types::runtime::AgentProvider::Codex,
                calm_types::worker::WorkerSessionState::Idle,
                None,
                serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
    }
    fn event(&self, subject: &str, text: &str) -> Event {
        Event::AssistantInputRequested {
            card_id: self.card.clone().into(),
            track_id: self.track.clone().into(),
            original_worker: self.worker.clone(),
            source: SOURCE,
            subject: subject.into(),
            version: 1,
            text: text.into(),
        }
    }
    async fn append(&self, tx: &mut Transaction<'_, Sqlite>, event: &Event) -> i64 {
        db::append_decision_event_in_tx(
            tx,
            &ActorId::Kernel,
            &EventScope::Card {
                card: self.card.clone().into(),
                track: self.track.clone().into(),
                area: self.area.clone().into(),
            },
            None,
            event,
        )
        .await
        .unwrap()
    }
    async fn accept(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        event: &Event,
    ) -> (i64, AcceptedPendingInput) {
        let id = self.append(tx, event).await;
        let (accepted, _) = system_assistant_accept_tx(tx, &self.card, id)
            .await
            .unwrap();
        (id, accepted)
    }
    async fn accept_raw(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        event: &Event,
    ) -> (i64, AcceptedPendingInput) {
        let id = self.append(tx, event).await;
        let fact = db::event_inputs::event_input_tx(tx, id).await.unwrap();
        let Event::AssistantInputRequested { text, .. } = event else {
            panic!()
        };
        let mut carrier = current_input_carrier_tx(tx, &self.card, &self.worker)
            .await
            .unwrap();
        let accepted = accept_pending_system_tx(
            tx,
            &mut carrier,
            calm_types::conversation::SystemObservation::try_from(Observation::SystemContext {
                text: text.clone(),
            })
            .unwrap(),
            &fact,
        )
        .await
        .unwrap();
        (id, accepted)
    }
}

#[tokio::test]
async fn additive_migration_preserves_original_database_identity_and_event() {
    use sqlx::migrate::Migrator;
    use std::borrow::Cow;
    let tmp = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("upgrade.db").display()
    );
    let old = Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 200)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    old.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO database_identity VALUES(1,'original-database',0)")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let id = db::append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Track {
            track: "original-track".into(),
            area: "original-area".into(),
        },
        None,
        &Event::TrackWakeRequested {
            track_id: "original-track".into(),
            source: "original".into(),
            key: "original".into(),
            text: "original".into(),
        },
    )
    .await
    .unwrap();
    let before = db::event_inputs::event_input_tx(&mut tx, id).await.unwrap();
    tx.commit().await.unwrap();
    // Upgrade uses the real migrator, without rewriting any released checksum.
    crate::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_system_assistant_entries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_system_assistant_receipts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        db::event_inputs::event_input_tx(&mut tx, id)
            .await
            .unwrap()
            .fingerprint,
        before.fingerprint
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM database_identity")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        "original-database"
    );
    tx.rollback().await.unwrap();
    pool.close().await;
}

#[tokio::test]
async fn human_and_system_creation_share_permanent_namespace_in_both_orders() {
    use calm_types::{
        conversation::*, conversation_receipt::*, organization::LEGACY_OWNER_USER_ID,
    };
    for system_first in [false, true] {
        let f = Fixture::new().await;
        let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
        if system_first {
            system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
                .await
                .unwrap();
        }
        let mut carrier = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
            .await
            .unwrap();
        let accepted = accept_pending_human_tx(
            &mut tx,
            &mut carrier,
            LEGACY_OWNER_USER_ID,
            HumanPayload {
                text: "human".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!()
        };
        let record = AssistantCreationRecord {
            card_id: f.card.clone(),
            track_id: f.track.clone(),
            request_sha256: "a".repeat(64),
            source_user_id: LEGACY_OWNER_USER_ID.into(),
            presentation_sequence: accepted.presentation_sequence,
            receipt: ConversationInitialInputReceipt {
                version: InitialInputReceiptVersion::V1,
                entry_id: message.message_id.as_str().into(),
                entry_rev: 1,
                queued_at_ms: carrier.checkpoint().pending[0].queued_at_ms(),
                text_sha256: text_digest("human"),
                initial_runtime_id: f.worker.clone(),
            },
        };
        let result = assistant_creation_insert_tx(&mut tx, &record).await;
        if system_first {
            assert!(matches!(result, Err(CalmError::Internal(_))));
        } else {
            result.unwrap();
            assert!(matches!(
                system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original").await,
                Err(CalmError::IdempotencyKeyExhausted(_))
            ));
        }
        assert!(assistant_key_used_tx(&mut tx, &f.card).await.unwrap());
        tx.rollback().await.unwrap();
    }
}

mod adversarial;
mod atomic;

mod scope;
