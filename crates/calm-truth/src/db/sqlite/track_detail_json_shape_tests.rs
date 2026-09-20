//! `track_detail` builds `cards` / `overlays` with sqlite's JSON constructors;
//! these pin the escaping, NULL, bool, empty-group and corrupt-payload shapes.

use super::{SqlxRepo, area_create_tx, card_create_tx, overlay_upsert_tx, track_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewArea, NewCard, NewOverlay, NewTrack, RequestTheme};
use serde_json::json;

/// Every escape hazard a TEXT column can carry: the two characters JSON must
/// escape, whitespace escapes, a raw C0 control character, multi-byte UTF-8.
const HOSTILE_TEXT: &str = "quote\" backslash\\ newline\n tab\t ctrl\u{1}\u{1f} 中文 🌊";

async fn empty_track(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "shape".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("area");
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "shape".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("track");
    tx.commit().await.expect("commit");
    track.id.to_string()
}

async fn add_card(repo: &SqlxRepo, track_id: &str, card: NewCard) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let created = card_create_tx(&mut tx, card, &CardRoleCache::new())
        .await
        .expect("card");
    tx.commit().await.expect("commit");
    debug_assert_eq!(created.track_id.as_str(), track_id);
    created.id.to_string()
}

#[tokio::test]
async fn track_detail_renders_empty_groups_as_empty_arrays() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail must succeed on a track with no cards")
        .expect("track exists");

    assert!(detail.cards.is_empty(), "cards: {:?}", detail.cards);
    assert!(
        detail.overlays.is_empty(),
        "overlays: {:?}",
        detail.overlays
    );
}

#[tokio::test]
async fn track_detail_round_trips_hostile_text_in_every_string_column() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let card_id = add_card(
        &repo,
        &track_id,
        NewCard {
            track_id: track_id.clone().into(),
            kind: format!("note{HOSTILE_TEXT}"),
            sort: Some(1.0),
            payload: json!({ HOSTILE_TEXT: HOSTILE_TEXT, "nested": [HOSTILE_TEXT] }),
            title: Some(HOSTILE_TEXT.to_string()),
        },
    )
    .await;

    let mut tx = repo.pool().begin().await.expect("begin");
    overlay_upsert_tx(
        &mut tx,
        NewOverlay {
            plugin_id: format!("plugin{HOSTILE_TEXT}"),
            entity_kind: "card".into(),
            entity_id: card_id.clone(),
            kind: format!("kind{HOSTILE_TEXT}"),
            payload: json!({ "text": HOSTILE_TEXT }),
        },
    )
    .await
    .expect("overlay");
    tx.commit().await.expect("commit");

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

    let card = detail.cards.first().expect("one card");
    assert_eq!(card.kind, format!("note{HOSTILE_TEXT}"));
    assert_eq!(card.title.as_deref(), Some(HOSTILE_TEXT));
    assert_eq!(card.payload[HOSTILE_TEXT], json!(HOSTILE_TEXT));
    assert_eq!(card.payload["nested"][0], json!(HOSTILE_TEXT));

    let overlay = detail.overlays.first().expect("one overlay");
    assert_eq!(overlay.plugin_id, format!("plugin{HOSTILE_TEXT}"));
    assert_eq!(overlay.kind, format!("kind{HOSTILE_TEXT}"));
    assert_eq!(overlay.entity_id, card_id);
    assert_eq!(overlay.payload["text"], json!(HOSTILE_TEXT));
}

#[tokio::test]
async fn track_detail_renders_null_title_as_none() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    add_card(
        &repo,
        &track_id,
        NewCard {
            track_id: track_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");
    assert_eq!(detail.cards.first().expect("one card").title, None);
}

#[tokio::test]
async fn track_detail_round_trips_deletable_both_ways() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let card_id = add_card(
        &repo,
        &track_id,
        NewCard {
            track_id: track_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    let deletable = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists")
        .cards
        .first()
        .expect("one card")
        .deletable;
    assert!(deletable, "cards default to deletable");

    sqlx::query("UPDATE cards SET deletable = 0 WHERE id = ?1")
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .expect("clear deletable");

    let deletable = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists")
        .cards
        .first()
        .expect("one card")
        .deletable;
    assert!(
        !deletable,
        "a system card must not read back as user-deletable"
    );
}

#[tokio::test]
async fn track_detail_round_trips_non_object_payloads() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let shapes = [
        json!({}),
        json!([]),
        json!(null),
        json!(true),
        json!(-0.5),
        json!("plain string"),
        json!({"deep": {"a": [1, {"b": null}]}}),
    ];
    for (i, payload) in shapes.iter().enumerate() {
        add_card(
            &repo,
            &track_id,
            NewCard {
                track_id: track_id.clone().into(),
                kind: "note".into(),
                sort: Some(i as f64),
                payload: payload.clone(),
                title: None,
            },
        )
        .await;
    }

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");
    let got: Vec<serde_json::Value> = detail.cards.iter().map(|c| c.payload.clone()).collect();
    assert_eq!(got, shapes, "payload must splice through byte-identically");
}

/// The fixture writes the column directly, the way disk corruption would, with
/// text crafted to close the card object and open another: a raw splice would
/// decode TWO cards; `json()` makes the whole statement error instead.
#[tokio::test]
async fn corrupt_payload_fails_the_read_instead_of_fabricating_a_card() {
    const FORGERY: &str =
        r#"{}},{"id":"forged","track_id":"forged","kind":"note","sort":9,"payload":{}"#;

    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let card_id = add_card(
        &repo,
        &track_id,
        NewCard {
            track_id: track_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
        .bind(FORGERY)
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .expect("a raw column write is exactly what corruption looks like");

    let err = repo
        .track_detail(&track_id)
        .await
        .expect_err("a corrupt card payload must surface as an error");
    assert!(
        err.to_string().to_lowercase().contains("json"),
        "unexpected error: {err}"
    );

    sqlx::query("UPDATE cards SET payload = \'{}\' WHERE id = ?1")
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .expect("restore the card payload");
    sqlx::query(
        "INSERT INTO overlays (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at) \
         VALUES (\'forged-overlay\', \'p\', \'card\', ?1, \'status\', ?2, 0)",
    )
    .bind(&card_id)
    .bind(FORGERY)
    .execute(repo.pool())
    .await
    .expect("seed a corrupt overlay payload");

    let err = repo
        .track_detail(&track_id)
        .await
        .expect_err("a corrupt overlay payload must surface as an error too");
    assert!(
        err.to_string().to_lowercase().contains("json"),
        "unexpected error: {err}"
    );
}
