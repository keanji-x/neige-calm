//! 0158 (#2348): every existing `ask.requested` becomes a `wake` ask and every existing
//! `ask.answered` answer becomes `{"text": ..}`, in order; all ask rows are stamped 27 and decode
//! as the typed events. Rows of other kinds and invalid payloads are left alone.

use calm_truth::MIGRATOR;
use calm_types::event::{AskAnswer, AskDelivery, Event};
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn schema_up_to(version: i64) -> SqliteConnection {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version <= version && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    db
}

async fn apply(db: &mut SqliteConnection, version: i64) {
    let migration = MIGRATOR
        .iter()
        .find(|m| m.version == version && !m.migration_type.is_down_migration())
        .unwrap();
    db.apply(migration).await.unwrap();
}

async fn insert(db: &mut SqliteConnection, kind: &str, payload: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_track) \
         VALUES(?1,?2,'{\"kind\":\"User\"}',1,26,'track','track') RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .fetch_one(&mut *db)
    .await
    .unwrap()
}

async fn row(db: &mut SqliteConnection, id: i64) -> (String, i64) {
    sqlx::query_as("SELECT payload, event_version FROM events WHERE id = ?1")
        .bind(id)
        .fetch_one(&mut *db)
        .await
        .unwrap()
}

#[tokio::test]
async fn existing_asks_become_wake_asks_with_text_answers() {
    let mut db = schema_up_to(157).await;
    let asked = insert(
        &mut db,
        "ask.requested",
        r#"{"track_id":"track","questions":[{"title":"Merge?","options":["Merge","Hold"]},{"title":"Branch?","options":[]}]}"#,
    )
    .await;
    let native = insert(
        &mut db,
        "ask.requested",
        r#"{"track_id":"track","questions":[{"title":"Which?","options":[]}],"source_item_id":"item-1"}"#,
    )
    .await;
    let answered = insert(
        &mut db,
        "ask.answered",
        &format!(r#"{{"ask_id":{asked},"track_id":"track","answers":["Merge","main"]}}"#),
    )
    .await;
    let garbage = insert(&mut db, "ask.answered", "not json").await;
    let other = insert(&mut db, "track.wake_requested", r#"{"answers":["x"]}"#).await;

    apply(&mut db, 158).await;

    let (payload, version) = row(&mut db, asked).await;
    assert_eq!(version, 27);
    match Event::from_kind_and_payload("ask.requested", serde_json::from_str(&payload).unwrap())
        .unwrap()
    {
        Event::AskRequested {
            delivery,
            questions,
            ..
        } => {
            assert_eq!(delivery, AskDelivery::Wake);
            assert_eq!(questions[0].options, vec!["Merge", "Hold"]);
        }
        other => panic!("{other:?}"),
    }
    let (payload, version) = row(&mut db, native).await;
    assert_eq!(version, 27);
    let native_payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(native_payload["delivery"], "wake");
    assert_eq!(native_payload["source_item_id"], "item-1");

    let (payload, version) = row(&mut db, answered).await;
    assert_eq!(version, 27);
    match Event::from_kind_and_payload("ask.answered", serde_json::from_str(&payload).unwrap())
        .unwrap()
    {
        Event::AskAnswered {
            ask_id, answers, ..
        } => {
            assert_eq!(ask_id, asked);
            assert_eq!(
                answers,
                vec![
                    AskAnswer::Text("Merge".into()),
                    AskAnswer::Text("main".into())
                ],
                "answers keep their order"
            );
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(row(&mut db, garbage).await, ("not json".to_string(), 27));
    assert_eq!(
        row(&mut db, other).await,
        (r#"{"answers":["x"]}"#.to_string(), 26),
        "another kind is not touched"
    );
}
