use super::{SqlxRepo, terminal_presentation::set_workbench_tx};
use crate::card_kind::prepare_card_create_payload_global;
use crate::db::RepoSyncDomainRaw;
use crate::model::{CardPatch, NewArea, NewCard, NewTrack, RequestTheme};
use calm_types::card_presentation::{
    CardCreationSource, TERMINAL_PRESENTATION_PAYLOAD_KEY, TerminalPresentation,
};
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use std::borrow::Cow;

fn through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[tokio::test]
async fn terminal_presentation_migration_covers_all_cards_without_changing_operation_bytes() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    through(172).run(&pool).await.unwrap();
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&pool)
        .await
        .unwrap();
    let cases = [
        (
            "manual",
            Some("terminal-create"),
            json!({"planner_hooks":false}),
            "manual",
            true,
        ),
        (
            "agent",
            Some("terminal-create"),
            json!({"planner_hooks":true}),
            "agent",
            false,
        ),
        (
            "task",
            Some("terminal-worker"),
            json!({"idempotency_key":"task"}),
            "task",
            false,
        ),
        (
            "typed",
            Some("terminal-execution-create"),
            json!({}),
            "manual",
            true,
        ),
        (
            "absent",
            Some("terminal-create"),
            json!({}),
            "unrecorded",
            false,
        ),
        (
            "generic",
            Some("card-create"),
            json!({}),
            "unrecorded",
            false,
        ),
        ("system", None, json!({}), "unrecorded", false),
        ("null", None, json!({}), "unrecorded", false),
        ("old-untrusted-key", None, json!({}), "unrecorded", false),
    ];
    for (id, kind, input, _, _) in &cases {
        let payload = if *id == "null" {
            Value::Null
        } else {
            json!({"schemaVersion":1,"preserved":"yes", "terminal_presentation":{"source":"manual","in_workbench":true}})
        };
        sqlx::query("INSERT INTO cards(id,track_id,kind,title,sort,payload,deletable,created_at,updated_at,role) \
            VALUES(?1,'track','terminal',NULL,0,?2,?3,7,11,'worker')")
            .bind(id).bind(serde_json::to_string(&payload).unwrap()).bind(if *id=="system" {0} else {1})
            .execute(&pool).await.unwrap();
        if let Some(kind) = kind {
            sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,\
                payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) \
                VALUES(?1,?1,?2,'frozen-hash','unknown','{\"type\":\"unknown\",\"id\":null}',?3,?4,'succeeded',1,1)")
                .bind(id).bind(kind).bind(serde_json::to_string(input).unwrap())
                .bind(serde_json::to_string(&json!({"result":{"id":id},"data":{"card_id":id}})).unwrap())
                .execute(&pool).await.unwrap();
        }
    }
    sqlx::query("INSERT INTO cards(id,track_id,kind,title,sort,payload,deletable,created_at,updated_at,role) \
            VALUES('other','track','codex',NULL,0,'null',1,7,11,'worker')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO cards(id,track_id,kind,title,sort,payload,deletable,created_at,updated_at,role) \
            VALUES('forged-other','track','codex',NULL,0,?1,1,7,11,'worker')")
        .bind(json!({"preserved":"yes","terminal_presentation":{"source":"manual","in_workbench":true}}).to_string())
        .execute(&pool).await.unwrap();
    let before: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id,payload_json,payload_hash,tx_output_json FROM operations ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    crate::MIGRATOR.run(&pool).await.unwrap();
    let after: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id,payload_json,payload_hash,tx_output_json FROM operations ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        after, before,
        "migration must preserve claim CAS bytes and creation receipts"
    );
    for (id, _, _, source, pinned) in &cases {
        let (payload, updated): (String, i64) =
            sqlx::query_as("SELECT payload,updated_at FROM cards WHERE id=?1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            payload[TERMINAL_PRESENTATION_PAYLOAD_KEY]["source"], *source,
            "{id}"
        );
        assert_eq!(
            payload[TERMINAL_PRESENTATION_PAYLOAD_KEY]["in_workbench"], *pinned,
            "{id}"
        );
        assert_eq!(
            updated, 11,
            "schema enrichment must not fake resource recency"
        );
        if *id != "null" {
            assert_eq!(payload["preserved"], "yes");
        }
    }
    let other: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id='other'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (forged, updated): (String, i64) =
        sqlx::query_as("SELECT payload,updated_at FROM cards WHERE id='forged-other'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let forged: Value = serde_json::from_str(&forged).unwrap();
    assert!(
        forged.get(TERMINAL_PRESENTATION_PAYLOAD_KEY).is_none(),
        "old client-authored provenance must not become sticky"
    );
    assert_eq!(forged["preserved"], "yes");
    assert_eq!(updated, 11);
    let converted =
        crate::card_kind::prepare_card_transition_payload_global("terminal", forged).unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&converted).unwrap(),
        TerminalPresentation::created(CardCreationSource::Unrecorded)
    );
    assert_eq!(
        other, "null",
        "other PTY-backed card kinds must be untouched"
    );
}

async fn fixture(source: CardCreationSource) -> (SqlxRepo, crate::model::Card) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "Terminal placement".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "Terminal placement".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            template_input: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let payload = prepare_card_create_payload_global(
        "terminal",
        json!({"schemaVersion":1,"preserved":"yes"}),
        source,
    )
    .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id,
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload,
        })
        .await
        .unwrap();
    (repo, card)
}

#[tokio::test]
async fn terminal_presentation_protected_replacements_and_placement_writer_preserve_source() {
    let (repo, card) = fixture(CardCreationSource::Manual).await;
    let original = TerminalPresentation::from_payload(&card.payload).unwrap();
    assert!(original.in_workbench);
    let mut tx = repo.pool().begin().await.unwrap();
    let moved = set_workbench_tx(&mut tx, card.id.as_str(), false)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&moved.payload).unwrap(),
        TerminalPresentation {
            source: CardCreationSource::Manual,
            in_workbench: false
        }
    );
    let updated=repo.card_update(card.id.as_str(),CardPatch{payload:Some(json!({"schemaVersion":1,"terminal_presentation":{"source":"agent","in_workbench":true}})),..Default::default()}).await.unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&updated.payload).unwrap(),
        TerminalPresentation {
            source: CardCreationSource::Manual,
            in_workbench: false
        }
    );
    let updated = repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                payload: Some(json!({"schemaVersion":1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&updated.payload)
            .unwrap()
            .source,
        CardCreationSource::Manual
    );
    assert!(
        !TerminalPresentation::from_payload(&updated.payload)
            .unwrap()
            .in_workbench
    );
    assert!(
        repo.card_update(
            card.id.as_str(),
            CardPatch {
                payload: Some(Value::Null),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    let mut tx = repo.pool().begin().await.unwrap();
    let restored = set_workbench_tx(&mut tx, card.id.as_str(), true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&restored.payload).unwrap(),
        original
    );
}

#[tokio::test]
async fn terminal_presentation_plugin_creation_and_unknown_transition_have_explicit_defaults() {
    let (repo, card) = fixture(CardCreationSource::Plugin).await;
    assert_eq!(
        TerminalPresentation::from_payload(&card.payload).unwrap(),
        TerminalPresentation {
            source: CardCreationSource::Plugin,
            in_workbench: false
        }
    );
    let other = repo
        .card_create(NewCard {
            track_id: card.track_id.clone(),
            title: None,
            kind: "plugin:example:view".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let converted = repo
        .card_update(
            other.id.as_str(),
            CardPatch {
                kind: Some("terminal".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&converted.payload).unwrap(),
        TerminalPresentation {
            source: CardCreationSource::Unrecorded,
            in_workbench: false
        }
    );
    let outgoing = repo
        .card_update(
            converted.id.as_str(),
            CardPatch {
                kind: Some("codex".into()),
                payload: Some(json!({"schemaVersion":1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        TerminalPresentation::from_payload(&outgoing.payload).unwrap(),
        TerminalPresentation::from_payload(&converted.payload).unwrap()
    );
}

#[tokio::test]
async fn terminal_presentation_corrupt_provenance_cannot_be_erased_or_repaired() {
    let (repo, card) = fixture(CardCreationSource::Agent).await;
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.terminal_presentation',json('null')) WHERE id=?1")
        .bind(card.id.as_str()).execute(repo.pool()).await.unwrap();
    let updated = repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                payload: Some(json!({"schemaVersion":1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        updated.payload.get(TERMINAL_PRESENTATION_PAYLOAD_KEY),
        Some(&Value::Null)
    );
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        set_workbench_tx(&mut tx, card.id.as_str(), true)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let outgoing = repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: Some("codex".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        outgoing.payload.get(TERMINAL_PRESENTATION_PAYLOAD_KEY),
        Some(&Value::Null)
    );
    assert!(
        repo.card_update(
            card.id.as_str(),
            CardPatch {
                kind: Some("terminal".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
}
