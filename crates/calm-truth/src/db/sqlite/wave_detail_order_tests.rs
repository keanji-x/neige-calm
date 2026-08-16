//! #1016 — `wave_detail` must return `cards` and `overlays` in a DETERMINATE
//! order.
//!
//! Both arrays are built with `json_group_array`, and sqlite documents an
//! aggregate's input order as *arbitrary* — not fixed by an ORDER BY in a
//! subquery, and free to change between releases
//! (`https://www.sqlite.org/lang_aggfunc.html`). `wave_detail` therefore
//! imposes the order after decoding, with a key that is TOTAL:
//!
//!   * `cards`    — `(sort, id)`; `id` is the PK
//!   * `overlays` — `(entity_kind, entity_id, plugin_id, kind)`; the table's
//!     UNIQUE key
//!
//! Totality is the whole argument. Sorting by a key on which no two distinct
//! rows tie yields the same sequence for EVERY input permutation, so whatever
//! order the aggregate hands over stops mattering. A non-unique key (`sort`
//! alone, which is what this code used to do) only permutes within tie groups
//! and leaves the rest to the scan.
//!
//! (The aggregate `ORDER BY` sqlite 3.44+ offers would work too — verified on
//! the bundled 3.46.0 — but `EXPLAIN QUERY PLAN` shows it always builds a
//! temp b-tree, ~28% on payload-heavy waves. See the note on `wave_detail`.)
//!
//! WHY THESE FIXTURES, and not the obvious ones. The trap here is a test that
//! is green either way:
//!
//!   * `cards` — `idx_cards_wave (wave_id, sort)` covers the scan, so cards
//!     with DISTINCT `sort` values come back sort-ordered even with no sorting
//!     on either side. Such a fixture proves nothing. The only entry where
//!     the aggregate's order is observable is a group of cards sharing ONE
//!     `sort`: inside the index they sit in rowid (= insertion) order, and the
//!     `id` tiebreak has to reorder them. Card ids are random uuid v4
//!     (`model::new_id`), so `seed_discriminating_tie_group` re-rolls until
//!     insertion order and id order actually differ, and asserts it — a fixture
//!     that happened to be seeded in id order would be a green-either-way test.
//!
//!   * `overlays` — every key column is chosen by the caller, so the fixture
//!     is deterministic outright: insert in an order that is the exact REVERSE
//!     of the key order on every column.
//!
//! Mutation-verified: weakening either key to a non-unique one — dropping the
//! `id` tiebreak from `cards`, dropping the `overlays` sort entirely — turns
//! the matching test red.

use super::{SqlxRepo, card_create_tx, cove_create_tx, overlay_upsert_tx, wave_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewCard, NewCove, NewOverlay, NewWave, RequestTheme};
use serde_json::json;

async fn empty_wave(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "order".into(),
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
            title: "order".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        repo.wave_cove_cache(),
    )
    .await
    .expect("wave");
    tx.commit().await.expect("commit");
    wave.id.to_string()
}

/// Create `n` cards on a fresh wave, all with the SAME `sort`, returning the
/// ids in INSERTION order.
async fn seed_tie_group(repo: &SqlxRepo, sort: f64, n: usize) -> (String, Vec<String>) {
    let wave_id = empty_wave(repo).await;
    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.expect("begin");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave_id.clone().into(),
                kind: "note".into(),
                sort: Some(sort),
                payload: json!({ "i": i }),
                title: Some(format!("card {i}")),
            },
            &role_cache,
        )
        .await
        .expect("card");
        ids.push(card.id.to_string());
    }
    tx.commit().await.expect("commit");
    (wave_id, ids)
}

/// A tie group whose insertion order is provably NOT its id order — the only
/// fixture shape that can tell the `(sort, id)` key apart from `sort` alone.
///
/// Ids are random, so this re-rolls (on a fresh wave, to keep the scan clean)
/// rather than hoping. Eight cards make a degenerate roll a 1-in-40320 event,
/// so the loop practically never spins; the cap keeps it from hanging if id
/// generation ever became monotonic — in which case the ASSERT below is the
/// honest failure, telling the next reader this test lost its teeth.
async fn seed_discriminating_tie_group(repo: &SqlxRepo, sort: f64) -> (String, Vec<String>) {
    const CARDS: usize = 8;
    for _ in 0..16 {
        let (wave_id, inserted) = seed_tie_group(repo, sort, CARDS).await;
        let mut by_id = inserted.clone();
        by_id.sort();
        if by_id != inserted {
            return (wave_id, inserted);
        }
    }
    panic!(
        "could not seed a tie group whose insertion order differs from its id \
         order; card ids may have become monotonic, which would make this \
         test green whether or not the `id` tiebreak is present"
    );
}

/// The claim under test, stated directly: the result must not depend on the
/// order sqlite feeds the aggregate.
///
/// "Arbitrary input order" is otherwise unobservable — one build, one plan,
/// one order. So this test CHANGES THE PLAN: it reads the wave, drops the two
/// indexes the subqueries scan through, and reads again. Without
/// `idx_cards_wave (wave_id, sort)` the cards scan becomes a full table scan
/// in rowid (insertion) order, and the fixture is inserted in descending
/// `sort` — so the aggregate genuinely receives the two arrays in different
/// orders across the two reads. Both must decode to the same sequence.
///
/// This is the "today green, tomorrow wrong" case made to happen today: a
/// future index, a schema change, or a sqlite upgrade picking another plan is
/// exactly the drop performed here.
#[tokio::test]
async fn wave_detail_order_survives_a_plan_change() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let role_cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.expect("begin");
    let mut card_ids = Vec::new();
    // Descending `sort`, so insertion (rowid) order is the REVERSE of index
    // order — the two plans disagree as loudly as possible.
    for sort in [5.0_f64, 4.0, 3.0, 3.0, 2.0, 1.0] {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave_id.clone().into(),
                kind: "note".into(),
                sort: Some(sort),
                payload: json!({}),
                title: None,
            },
            &role_cache,
        )
        .await
        .expect("card");
        card_ids.push(card.id.to_string());
    }
    for (i, cid) in card_ids.iter().enumerate() {
        overlay_upsert_tx(
            &mut tx,
            NewOverlay {
                plugin_id: format!("p{}", 9 - i),
                entity_kind: "card".into(),
                entity_id: cid.clone(),
                kind: "status".into(),
                payload: json!({}),
            },
        )
        .await
        .expect("overlay");
    }
    tx.commit().await.expect("commit");

    async fn snapshot(repo: &SqlxRepo, wave_id: &str) -> (Vec<String>, Vec<String>) {
        let d = repo
            .wave_detail(wave_id)
            .await
            .expect("wave_detail")
            .expect("wave exists");
        (
            d.cards.iter().map(|c| c.id.to_string()).collect(),
            d.overlays.iter().map(|o| o.id.clone()).collect(),
        )
    }

    let before = snapshot(&repo, &wave_id).await;

    for stmt in [
        "DROP INDEX idx_cards_wave",
        "DROP INDEX idx_overlays_entity",
    ] {
        sqlx::query(stmt)
            .execute(repo.pool())
            .await
            .expect("drop index");
    }

    let after = snapshot(&repo, &wave_id).await;
    assert_eq!(
        after, before,
        "wave_detail must return the same order after the scan plan changed. \
         A difference means the order was the query plan's, not the data's."
    );
    assert_eq!(before.0.len(), card_ids.len(), "all cards present");
    assert_eq!(before.1.len(), card_ids.len(), "all overlays present");
}

/// Cards sharing one `sort` must come back in `id` order — the tiebreak that
/// the index scan alone does not provide.
#[tokio::test]
async fn wave_detail_orders_tied_cards_by_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let (wave_id, inserted) = seed_discriminating_tie_group(&repo, 1.0).await;

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

    let got: Vec<String> = detail.cards.iter().map(|c| c.id.to_string()).collect();
    let mut want = inserted.clone();
    want.sort();
    assert_eq!(
        got, want,
        "cards with an identical `sort` must be ordered by `id`. \
         got {got:?}, want {want:?} (insertion order was {inserted:?}). \
         A result equal to the insertion order means the aggregate is taking \
         its input in scan order — the order sqlite documents as arbitrary."
    );
}

/// The whole key, not just the tiebreak: distinct `sort` values order ascending
/// and ties fall through to `id`, in one array.
#[tokio::test]
async fn wave_detail_orders_cards_by_sort_then_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let role_cache = CardRoleCache::new();

    // Seeded in DESCENDING sort so the sort key has work to do, with a tie at
    // 2.0 so the id tiebreak does too.
    let mut tx = repo.pool().begin().await.expect("begin");
    let mut seeded: Vec<(f64, String)> = Vec::new();
    for sort in [3.0_f64, 2.0, 2.0, 2.0, 1.0] {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave_id.clone().into(),
                kind: "note".into(),
                sort: Some(sort),
                payload: json!({}),
                title: None,
            },
            &role_cache,
        )
        .await
        .expect("card");
        seeded.push((sort, card.id.to_string()));
    }
    tx.commit().await.expect("commit");

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

    let mut want = seeded.clone();
    want.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let got: Vec<(f64, String)> = detail
        .cards
        .iter()
        .map(|c| (c.sort, c.id.to_string()))
        .collect();
    assert_eq!(
        got, want,
        "cards must come back ordered by (sort ASC, id ASC)"
    );
}

/// Overlays must come back in UNIQUE-key order. Deterministic by construction:
/// every row is inserted at the position the key order will move it away from.
#[tokio::test]
async fn wave_detail_orders_overlays_by_unique_key() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let wave_id = empty_wave(&repo).await;
    let role_cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.expect("begin");
    let mut card_ids = Vec::new();
    for i in 0..2 {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave_id.clone().into(),
                kind: "note".into(),
                sort: Some(i as f64),
                payload: json!({}),
                title: None,
            },
            &role_cache,
        )
        .await
        .expect("card");
        card_ids.push(card.id.to_string());
    }
    tx.commit().await.expect("commit");
    // `entity_id` is a random uuid, so derive the expectation from the values
    // rather than assuming which card sorts first.
    card_ids.sort();

    // Insertion order is the exact reverse of the key order on every column:
    // `wave` before `card` (entity_kind descending), the higher `entity_id`
    // first, then `plugin_id` z→a and `kind` z→a.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        ("wave", wave_id.clone(), "zeta", "z-kind"),
        ("wave", wave_id.clone(), "zeta", "a-kind"),
        ("wave", wave_id.clone(), "alpha", "z-kind"),
        ("card", card_ids[1].clone(), "zeta", "z-kind"),
        ("card", card_ids[1].clone(), "alpha", "a-kind"),
        ("card", card_ids[0].clone(), "zeta", "a-kind"),
        ("card", card_ids[0].clone(), "alpha", "z-kind"),
        ("card", card_ids[0].clone(), "alpha", "a-kind"),
    ];

    let mut tx = repo.pool().begin().await.expect("begin");
    for (entity_kind, entity_id, plugin_id, kind) in &rows {
        overlay_upsert_tx(
            &mut tx,
            NewOverlay {
                plugin_id: (*plugin_id).to_string(),
                entity_kind: (*entity_kind).to_string(),
                entity_id: entity_id.clone(),
                kind: (*kind).to_string(),
                payload: json!({}),
            },
        )
        .await
        .expect("overlay");
    }
    tx.commit().await.expect("commit");

    let inserted: Vec<(String, String, String, String)> = rows
        .iter()
        .map(|(ek, ei, p, k)| {
            (
                (*ek).to_string(),
                ei.clone(),
                (*p).to_string(),
                (*k).to_string(),
            )
        })
        .collect();
    let mut want = inserted.clone();
    want.sort();
    assert_ne!(
        want, inserted,
        "fixture guard: insertion order must differ from key order, or this \
         test cannot tell the aggregate ORDER BY from scan order"
    );

    let detail = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists");

    let got: Vec<(String, String, String, String)> = detail
        .overlays
        .iter()
        .map(|o| {
            (
                o.entity_kind.clone(),
                o.entity_id.clone(),
                o.plugin_id.clone(),
                o.kind.clone(),
            )
        })
        .collect();
    assert_eq!(
        got.len(),
        rows.len(),
        "every seeded overlay must be in the result"
    );
    assert_eq!(
        got, want,
        "overlays must come back ordered by (entity_kind, entity_id, \
         plugin_id, kind) — the table's UNIQUE key. Anything else is the \
         arbitrary aggregate input order."
    );
}

/// The order must not depend on WHICH invocation it is: sqlite is free to feed
/// an aggregate differently between calls, so the same wave read repeatedly
/// must produce byte-identical sequences.
#[tokio::test]
async fn wave_detail_order_is_stable_across_repeated_reads() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let (wave_id, _) = seed_discriminating_tie_group(&repo, 7.5).await;

    let first: Vec<String> = repo
        .wave_detail(&wave_id)
        .await
        .expect("wave_detail")
        .expect("wave exists")
        .cards
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    for _ in 0..8 {
        let again: Vec<String> = repo
            .wave_detail(&wave_id)
            .await
            .expect("wave_detail")
            .expect("wave exists")
            .cards
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        assert_eq!(again, first, "wave_detail order must be reproducible");
    }
}
