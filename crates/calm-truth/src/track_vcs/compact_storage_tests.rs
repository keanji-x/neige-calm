use super::store::store_tree;
use super::transcript_writer_tests::Fixture;
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{append_decision_event_in_tx, begin_immediate_tx, card_update_tx};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::CardPatch;
use serde_json::json;
use std::collections::BTreeMap;

#[tokio::test]
async fn large_tree_keeps_manifest_with_smaller_storage() {
    let f = Fixture::new().await;
    let entries: BTreeMap<_, _> = (0..256)
        .map(|n| {
            (
                format!("cards/{n:032x}/runtime.json"),
                ManifestEntry {
                    blob_hash: format!("{n:064x}"),
                    byte_len: 123,
                    content_type: "application/json".into(),
                },
            )
        })
        .collect();
    let expected = TreeManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        entries: entries.clone(),
    };
    let raw = canonical_json_bytes(&expected).unwrap();
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let tree = store_tree(&mut tx, MANIFEST_SCHEMA_VERSION, entries, 1)
        .await
        .unwrap();
    let stored: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM track_vcs_objects WHERE hash=?1")
        .bind(&tree.tree_hash)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert!(
        stored.len() < raw.len() / 2,
        "stored={} logical={}",
        stored.len(),
        raw.len()
    );
    assert_eq!(tree.manifest, expected);
    let loaded = super::store::load_tree_object_pool(f.repo.pool(), &tree.tree_hash);
    tx.commit().await.unwrap();
    assert_eq!(loaded.await.unwrap(), Some(expected));
}

#[tokio::test]
async fn large_card_event_keeps_replay_content_with_smaller_persisted_payload() {
    let f = Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    let updated = card_update_tx(
        &mut tx,
        f.card.id.as_str(),
        CardPatch {
            payload: Some(
                json!({"schemaVersion":1,"text":"Repeated report content. ".repeat(4096)}),
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let event = Event::CardUpdated(updated);
    let logical = serde_json::to_value(&event).unwrap();
    let plain = serde_json::to_vec(&event.payload_value()).unwrap();
    let id = append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Card {
            card: f.card.id.clone(),
            track: f.track.id.clone(),
            area: f.track.area_id.clone(),
        },
        None,
        &event,
    )
    .await
    .unwrap();
    let stored: String = sqlx::query_scalar("SELECT payload FROM events WHERE id=?1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert!(
        stored.len() < plain.len() / 2,
        "stored={} logical={}",
        stored.len(),
        plain.len()
    );
    let metadata: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(metadata["id"], json!(f.card.id));
    let fact = crate::db::sqlite::event_inputs::event_input_tx(&mut tx, id)
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(fact.envelope.event).unwrap(), logical);
    let fingerprint = fact.fingerprint;
    tx.commit().await.unwrap();
    let replay = f.repo.events_since(id - 1, 1).await.unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(serde_json::to_value(&replay[0].3).unwrap(), logical);
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        crate::db::sqlite::event_inputs::event_input_tx(&mut tx, id)
            .await
            .unwrap()
            .fingerprint,
        fingerprint
    );
    sqlx::query(
        "UPDATE events SET payload=json_set(payload,'$._storage_fields.version',2) WHERE id=?1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await
    .unwrap();
    assert!(
        crate::db::sqlite::event_inputs::event_input_tx(&mut tx, id)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    assert!(f.repo.events_since(id - 1, 1).await.is_err());
}
