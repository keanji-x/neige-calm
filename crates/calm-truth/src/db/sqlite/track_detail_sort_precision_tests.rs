//! `track_detail` must carry `cards.sort` back **bit-exactly**: sqlite's
//! `json_object` renders a FLOAT with only 15 significant digits and binary64
//! needs 17, so the read renders via `printf('%!.17g', …)`.

use super::{SqlxRepo, area_create_tx, card_create_tx, track_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use serde_json::json;

/// f64 values that cannot survive 15-significant-digit rendering; the test
/// re-derives that property before touching the database.
const PRECISION_HOSTILE_SORTS: &[f64] = &[
    1.000_000_000_000_000_2,
    0.300_000_000_000_000_04,
    -1.000_000_000_000_000_2,
    123_456_789_012_345_680.0,
    f64::MIN_POSITIVE,
];

async fn seed_track_with_sorts(repo: &SqlxRepo, sorts: &[f64]) -> (String, Vec<String>) {
    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "sort precision".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "sort precision".into(),
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
    .expect("create track");
    let mut ids = Vec::with_capacity(sorts.len());
    for (i, sort) in sorts.iter().enumerate() {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track.id.clone(),
                kind: "note".into(),
                sort: Some(*sort),
                payload: json!({"i": i}),
                title: Some(format!("card {i}")),
            },
            &role_cache,
        )
        .await
        .expect("create card");
        ids.push(card.id.to_string());
    }
    tx.commit().await.expect("commit seed");
    (track.id.to_string(), ids)
}

#[tokio::test]
async fn track_detail_round_trips_card_sort_bit_exactly() {
    // Guard the fixture itself: a value that survives 15 digits would pass for the wrong reason.
    for sort in PRECISION_HOSTILE_SORTS {
        let fifteen: f64 = format!("{sort:.14e}")
            .parse()
            .expect("parse 15-digit render");
        assert_ne!(
            fifteen.to_bits(),
            sort.to_bits(),
            "fixture value {sort:?} survives 15 significant digits, so it \
             cannot detect the `%!0.15g` rounding this test exists for"
        );
    }

    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let (track_id, card_ids) = seed_track_with_sorts(&repo, PRECISION_HOSTILE_SORTS).await;

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

    assert_eq!(detail.cards.len(), PRECISION_HOSTILE_SORTS.len());
    for (id, expected) in card_ids.iter().zip(PRECISION_HOSTILE_SORTS) {
        let card = detail
            .cards
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap_or_else(|| panic!("card {id} missing from track_detail"));
        assert_eq!(
            card.sort.to_bits(),
            expected.to_bits(),
            "card {id}: track_detail must return `sort` bit-exactly. \
             stored {expected:?} (bits {:#018x}), got {:?} (bits {:#018x}). \
             A mismatch here is the silent `json_object` FLOAT rounding — \
             nothing errors, the value is simply different, and the web \
             client writes it back to the DB on the next reorder.",
            expected.to_bits(),
            card.sort,
            card.sort.to_bits(),
        );
    }
}

#[tokio::test]
async fn track_detail_keeps_one_ulp_apart_sorts_distinct_and_ordered() {
    let lower = 1.0_f64;
    let upper = f64::from_bits(lower.to_bits() + 1);
    assert_ne!(lower, upper, "fixture must be two distinct f64s");

    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let (track_id, card_ids) = seed_track_with_sorts(&repo, &[upper, lower]).await;

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

    let sorts: Vec<u64> = detail.cards.iter().map(|c| c.sort.to_bits()).collect();
    assert_eq!(
        sorts,
        vec![lower.to_bits(), upper.to_bits()],
        "cards one ULP apart must stay distinct and sort ascending; a \
         rounded read collapses both onto 1.0 and the order goes arbitrary"
    );
    assert_eq!(
        detail.cards[0].id.as_str(),
        card_ids[1],
        "the card stored with the SMALLER sort must come first"
    );
}
