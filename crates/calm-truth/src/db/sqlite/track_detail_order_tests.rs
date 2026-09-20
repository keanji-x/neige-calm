//! `track_detail` must return `cards` and `overlays` in a DETERMINATE order:
//! sqlite feeds `json_group_array` in arbitrary order, so the read sorts by a
//! TOTAL key. Cards with distinct `sort` come back ordered from the index alone,
//! so only a tie group whose insertion order differs from id order can
//! discriminate `(sort, id)` from `sort`.

use super::{SqlxRepo, area_create_tx, card_create_tx, overlay_upsert_tx, track_create_tx};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoRead;
use crate::model::{NewArea, NewCard, NewOverlay, NewTrack, RequestTheme};
use serde_json::json;

async fn empty_track(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.expect("begin");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "order".into(),
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
            title: "order".into(),
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

async fn seed_tie_group(repo: &SqlxRepo, sort: f64, n: usize) -> (String, Vec<String>) {
    let track_id = empty_track(repo).await;
    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.expect("begin");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track_id.clone().into(),
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
    (track_id, ids)
}

/// A tie group whose insertion order is provably NOT its id order. Ids are
/// random, so this re-rolls; the cap keeps it from hanging if id generation
/// ever became monotonic, in which case the ASSERT is the honest failure.
async fn seed_discriminating_tie_group(repo: &SqlxRepo, sort: f64) -> (String, Vec<String>) {
    const CARDS: usize = 8;
    for _ in 0..16 {
        let (track_id, inserted) = seed_tie_group(repo, sort, CARDS).await;
        let mut by_id = inserted.clone();
        by_id.sort();
        if by_id != inserted {
            return (track_id, inserted);
        }
    }
    panic!(
        "could not seed a tie group whose insertion order differs from its id \
         order; card ids may have become monotonic, which would make this \
         test green whether or not the `id` tiebreak is present"
    );
}

/// Makes "arbitrary input order" observable by CHANGING THE PLAN: read, drop
/// the two indexes the subqueries scan through, read again. Without the index
/// the scan is rowid order, the reverse of the descending-sort fixture.
#[tokio::test]
async fn track_detail_order_survives_a_plan_change() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let role_cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.expect("begin");
    let mut card_ids = Vec::new();
    for sort in [5.0_f64, 4.0, 3.0, 3.0, 2.0, 1.0] {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track_id.clone().into(),
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

    async fn snapshot(repo: &SqlxRepo, track_id: &str) -> (Vec<String>, Vec<String>) {
        let d = repo
            .track_detail(track_id)
            .await
            .expect("track_detail")
            .expect("track exists");
        (
            d.cards.iter().map(|c| c.id.to_string()).collect(),
            d.overlays.iter().map(|o| o.id.clone()).collect(),
        )
    }

    let before = snapshot(&repo, &track_id).await;

    for stmt in [
        "DROP INDEX idx_cards_track",
        "DROP INDEX idx_overlays_entity",
    ] {
        sqlx::query(stmt)
            .execute(repo.pool())
            .await
            .expect("drop index");
    }

    let after = snapshot(&repo, &track_id).await;
    assert_eq!(
        after, before,
        "track_detail must return the same order after the scan plan changed. \
         A difference means the order was the query plan's, not the data's."
    );
    assert_eq!(before.0.len(), card_ids.len(), "all cards present");
    assert_eq!(before.1.len(), card_ids.len(), "all overlays present");
}

#[tokio::test]
async fn track_detail_orders_tied_cards_by_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let (track_id, inserted) = seed_discriminating_tie_group(&repo, 1.0).await;

    let detail = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

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

#[tokio::test]
async fn track_detail_orders_cards_by_sort_then_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let role_cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.expect("begin");
    let mut seeded: Vec<(f64, String)> = Vec::new();
    for sort in [3.0_f64, 2.0, 2.0, 2.0, 1.0] {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track_id.clone().into(),
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
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

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

#[tokio::test]
async fn track_detail_orders_overlays_by_unique_key() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let track_id = empty_track(&repo).await;
    let role_cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.expect("begin");
    let mut card_ids = Vec::new();
    for i in 0..2 {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track_id.clone().into(),
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
    // `entity_id` is a random uuid, so derive the expectation from the values.
    card_ids.sort();

    // Insertion order is the exact reverse of the key order on every column.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        ("track", track_id.clone(), "zeta", "z-kind"),
        ("track", track_id.clone(), "zeta", "a-kind"),
        ("track", track_id.clone(), "alpha", "z-kind"),
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
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists");

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

#[tokio::test]
async fn track_detail_order_is_stable_across_repeated_reads() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
    let (track_id, _) = seed_discriminating_tie_group(&repo, 7.5).await;

    let first: Vec<String> = repo
        .track_detail(&track_id)
        .await
        .expect("track_detail")
        .expect("track exists")
        .cards
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    for _ in 0..8 {
        let again: Vec<String> = repo
            .track_detail(&track_id)
            .await
            .expect("track_detail")
            .expect("track exists")
            .cards
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        assert_eq!(again, first, "track_detail order must be reproducible");
    }
}
