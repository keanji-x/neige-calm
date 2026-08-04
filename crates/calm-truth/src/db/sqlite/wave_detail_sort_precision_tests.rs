//! #1016 — `wave_detail` must carry `cards.sort` back **bit-exactly**.
//!
//! `wave_detail` is one statement that ships `cards` as a `json_group_array`
//! blob. `cards.sort` is a REAL column and `f64` in the model, so it crosses
//! a decimal-text boundary that the previous three-SELECT shape did not have.
//!
//! The hazard, in the bundled sqlite (libsqlite3-sys 0.30.1 → SQLite 3.46.0):
//! `jsonAppendSqlValue` renders a FLOAT argument with `%!0.15g` and — unlike
//! `sqlite3QuoteValue`, which reparses its output and falls back to
//! `%!0.20e` when the round-trip fails — has no fallback. binary64 needs 17
//! significant digits to round-trip, so `json_object('s', x)` silently
//! rounds: `1.0000000000000002` comes back as `1.0`.
//!
//! Nothing errors when that happens. `serde_json` deserializes the rounded
//! decimal happily, `cards.sort_by(total_cmp)` then orders two neighbouring
//! sorts wrong, and the web client persists what it read — `WaveList.tsx`
//! writes the displayed `sort` back through `PATCH /api/cards/:id` on
//! reorder, turning a display-only rounding into an unlogged rewrite of
//! stored data (and possibly a collision with another card's sort).
//!
//! The fix renders `sort` via `printf('%!.17g', …)` and splices it in as a
//! JSON number with `json()`. These tests are the pin. They are RED against
//! a plain `'sort', c.sort`.
//!
//! Assertions compare `f64::to_bits`, not `==`: `==` on floats would still
//! be the right verdict here, but bit comparison states the intent (and
//! keeps `-0.0` honest).

use super::{SqlxRepo, card_create_tx, cove_create_tx, wave_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewCard, NewCove, NewWave, RequestTheme};
use serde_json::json;

/// f64 values that cannot survive 15-significant-digit rendering.
///
/// Every entry is chosen so that a 15-significant-digit render (`{:.14e}`)
/// collapses it onto a DIFFERENT f64 — i.e. each one is a live
/// counterexample, not decoration; the test re-derives that property before
/// touching the database. `1.0000000000000002` is the ULP-successor of `1.0`;
/// `0.30000000000000004` is the canonical `0.1 + 0.2` repeat; the negative
/// covers sign, and the last two cover the large-magnitude and denormal ends
/// of the range a `PATCH /api/cards/:id` body can carry.
const PRECISION_HOSTILE_SORTS: &[f64] = &[
    1.000_000_000_000_000_2,
    0.300_000_000_000_000_04,
    -1.000_000_000_000_000_2,
    123_456_789_012_345_680.0,
    f64::MIN_POSITIVE,
];

async fn seed_wave_with_sorts(repo: &SqlxRepo, sorts: &[f64]) -> (String, Vec<String>) {
    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "sort precision".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create cove");
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id,
            title: "sort precision".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    let mut ids = Vec::with_capacity(sorts.len());
    for (i, sort) in sorts.iter().enumerate() {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave.id.clone(),
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
    (wave.id.to_string(), ids)
}

/// The headline pin: every hostile `sort` comes back from `wave_detail`
/// bit-identical to what was stored.
#[tokio::test]
async fn wave_detail_round_trips_card_sort_bit_exactly() {
    // Guard the fixture itself: a value that survives 15 digits would make
    // this test pass for the wrong reason.
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
    let (wave_id, card_ids) = seed_wave_with_sorts(&repo, PRECISION_HOSTILE_SORTS).await;

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

    assert_eq!(detail.cards.len(), PRECISION_HOSTILE_SORTS.len());
    for (id, expected) in card_ids.iter().zip(PRECISION_HOSTILE_SORTS) {
        let card = detail
            .cards
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap_or_else(|| panic!("card {id} missing from wave_detail"));
        assert_eq!(
            card.sort.to_bits(),
            expected.to_bits(),
            "card {id}: wave_detail must return `sort` bit-exactly. \
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

/// The consequence the round-trip protects: two f64s one ULP apart must stay
/// distinct and stay ordered. Under 15-digit rendering both collapse onto
/// `1.0`, `total_cmp` sees a tie, and the displayed order becomes arbitrary.
#[tokio::test]
async fn wave_detail_keeps_one_ulp_apart_sorts_distinct_and_ordered() {
    let lower = 1.0_f64;
    let upper = f64::from_bits(lower.to_bits() + 1);
    assert_ne!(lower, upper, "fixture must be two distinct f64s");

    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    // Seeded in the WRONG order so the read path has to reorder them.
    let (wave_id, card_ids) = seed_wave_with_sorts(&repo, &[upper, lower]).await;

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

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
