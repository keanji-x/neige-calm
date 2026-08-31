//! #1016 — `wave_detail` ships `cards` / `overlays` as
//! `json_group_array(json_object(…))`, so the array is built by sqlite's JSON
//! constructors rather than by string concatenation. This file pins the
//! guarantees that choice buys, against the real statement:
//!
//!   * TEXT escaping — quotes, backslashes, newlines, control characters,
//!     non-ASCII,
//!   * NULL `title` -> JSON `null`,
//!   * `deletable` as a JSON keyword, not sqlite's 0/1,
//!   * an empty `cards` / `overlays` group rendering `[]`, not `null`,
//!   * every `payload` shape round-tripping unchanged,
//!   * and the constructive safety property itself: a `payload` that is not
//!     valid JSON makes the read FAIL, it can never turn into card
//!     structure.
//!
//! (`sort` precision has its own file, `wave_detail_sort_precision_tests`;
//! ordering has `wave_detail_order_tests`.)

use super::{SqlxRepo, card_create_tx, cove_create_tx, overlay_upsert_tx, wave_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewCard, NewCove, NewOverlay, NewWave, RequestTheme};
use serde_json::json;

/// Every escape hazard a TEXT column can carry into hand-built JSON: the two
/// characters JSON itself must escape, the whitespace escapes, a C0 control
/// character (which JSON forbids raw), and multi-byte UTF-8.
const HOSTILE_TEXT: &str = "quote\" backslash\\ newline\n tab\t ctrl\u{1}\u{1f} 中文 🌊";

async fn empty_wave(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "shape".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("cove");
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id,
            title: "shape".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_cove_cache(),
    )
    .await
    .expect("wave");
    tx.commit().await.expect("commit");
    wave.id.to_string()
}

async fn add_card(repo: &SqlxRepo, wave_id: &str, card: NewCard) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let created = card_create_tx(&mut tx, card, &CardRoleCache::new())
        .await
        .expect("card");
    tx.commit().await.expect("commit");
    debug_assert_eq!(created.wave_id.as_str(), wave_id);
    created.id.to_string()
}

/// A wave with no cards and no overlays must come back as two EMPTY vectors.
/// `json_group_array` over zero rows is `[]` rather than `null` — a `null`
/// there would fail to deserialize into `Vec<_>`.
#[tokio::test]
async fn wave_detail_renders_empty_groups_as_empty_arrays() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail must succeed on a wave with no cards")
        .expect("wave exists");

    assert!(detail.cards.is_empty(), "cards: {:?}", detail.cards);
    assert!(
        detail.overlays.is_empty(),
        "overlays: {:?}",
        detail.overlays
    );
}

/// Every TEXT that crosses the aggregated-JSON boundary — card `kind`,
/// `title`, overlay `kind`/`plugin_id`, and strings nested inside both
/// `payload` columns — must round-trip byte-for-byte.
#[tokio::test]
async fn wave_detail_round_trips_hostile_text_in_every_string_column() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let card_id = add_card(
        &repo,
        &wave_id,
        NewCard {
            wave_id: wave_id.clone().into(),
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
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

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

/// A NULL `title` column must render JSON `null` (i.e. `Option::None`), not
/// an empty string.
#[tokio::test]
async fn wave_detail_renders_null_title_as_none() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    add_card(
        &repo,
        &wave_id,
        NewCard {
            wave_id: wave_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");
    assert_eq!(detail.cards.first().expect("one card").title, None);
}

/// `deletable` is 0/1 in sqlite and `bool` in the model; both states must
/// survive as JSON keywords. `false` is the security-relevant one (#229) —
/// a card that reads back `true` becomes deletable through the REST surface.
#[tokio::test]
async fn wave_detail_round_trips_deletable_both_ways() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let card_id = add_card(
        &repo,
        &wave_id,
        NewCard {
            wave_id: wave_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    let deletable = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists")
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
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists")
        .cards
        .first()
        .expect("one card")
        .deletable;
    assert!(
        !deletable,
        "a system card must not read back as user-deletable"
    );
}

/// `payload` is not always an object: the column stores whatever
/// `serde_json::Value` the writer had. Every JSON shape must splice through
/// unchanged.
#[tokio::test]
async fn wave_detail_round_trips_non_object_payloads() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
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
            &wave_id,
            NewCard {
                wave_id: wave_id.clone().into(),
                kind: "note".into(),
                sort: Some(i as f64),
                payload: payload.clone(),
                title: None,
            },
        )
        .await;
    }

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");
    let got: Vec<serde_json::Value> = detail.cards.iter().map(|c| c.payload.clone()).collect();
    assert_eq!(got, shapes, "payload must splice through byte-identically");
}

/// The constructive-safety property of `json_object` / `json()`, and the
/// reason the hand-assembled `printf` + raw-splice variant was reverted
/// (#1016): a `payload` that is not valid JSON makes the read FAIL LOUDLY.
///
/// The fixture writes the column directly, the way disk corruption, a
/// hand-edited row or a restored bad backup would — no application writer is
/// involved, so no write-side trigger could stand in the way. The text is
/// crafted to *close* the card object and open another one, which is exactly
/// what a raw splice would have obeyed: the array would have decoded into TWO
/// cards, one of them fabricated, with no error anywhere. Because the
/// statement routes `payload` through `json()`, sqlite parses it and the
/// whole statement errors instead — the behaviour
/// `spec_harness_wave_vcs::transcript_refresh_failure_from_corrupt_card_payload_does_not_wedge_harness`
/// relies on to degrade gracefully.
#[tokio::test]
async fn corrupt_payload_fails_the_read_instead_of_fabricating_a_card() {
    const FORGERY: &str =
        r#"{}},{"id":"forged","wave_id":"forged","kind":"note","sort":9,"payload":{}"#;

    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let card_id = add_card(
        &repo,
        &wave_id,
        NewCard {
            wave_id: wave_id.clone().into(),
            kind: "note".into(),
            sort: Some(1.0),
            payload: json!({}),
            title: None,
        },
    )
    .await;

    // Nothing stops the corrupt bytes from reaching the column ...
    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
        .bind(FORGERY)
        .bind(&card_id)
        .execute(repo.pool())
        .await
        .expect("a raw column write is exactly what corruption looks like");

    // ... and the read refuses to interpret them.
    let err = repo
        .wave_detail(&wave_id)
        .await
        .expect_err("a corrupt card payload must surface as an error");
    assert!(
        err.to_string().to_lowercase().contains("json"),
        "unexpected error: {err}"
    );

    // Same fence on the other table.
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
        .wave_detail(&wave_id)
        .await
        .expect_err("a corrupt overlay payload must surface as an error too");
    assert!(
        err.to_string().to_lowercase().contains("json"),
        "unexpected error: {err}"
    );
}
