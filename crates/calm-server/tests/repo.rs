//! Integration tests for `SqlxRepo` against an in-memory SQLite.
//!
//! These tests exercise the observable contract of the `Repo` trait against
//! the real sqlx-backed implementation: CRUD round-trips, cascade deletes,
//! sort defaulting, `wave_detail` composition, overlay upsert idempotency,
//! and terminal-per-card uniqueness.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::CalmError;
use calm_server::model::*;
use serde_json::json;

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn make_cove(repo: &SqlxRepo, name: &str) -> Cove {
    repo.cove_create(NewCove {
        name: name.into(),
        color: "#abcdef".into(),
        sort: None,
    })
    .await
    .expect("create cove")
}

async fn make_wave(repo: &SqlxRepo, cove_id: &str, title: &str) -> Wave {
    repo.wave_create(NewWave {
        cove_id: cove_id.into(),
        title: title.into(),
        sort: None,
    })
    .await
    .expect("create wave")
}

async fn make_card(repo: &SqlxRepo, wave_id: &str, kind: &str) -> Card {
    repo.card_create(NewCard {
        wave_id: wave_id.into(),
        kind: kind.into(),
        sort: None,
        payload: json!({"hello": "world"}),
    })
    .await
    .expect("create card")
}

// ---------------------------------------------------------------- CRUD ----

#[tokio::test]
async fn cove_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "Personal").await;
    assert_eq!(c.name, "Personal");

    let got = repo
        .cove_get(c.id.as_str())
        .await
        .unwrap()
        .expect("cove exists");
    assert_eq!(got.id, c.id);

    let listed = repo.coves_list().await.unwrap();
    assert_eq!(listed.len(), 1);

    let updated = repo
        .cove_update(
            c.id.as_str(),
            CovePatch {
                name: Some("Work".into()),
                color: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Work");
    assert_eq!(updated.color, c.color);

    repo.cove_delete(c.id.as_str()).await.unwrap();
    assert!(repo.cove_get(c.id.as_str()).await.unwrap().is_none());

    let err = repo.cove_delete(c.id.as_str()).await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
    let err = repo
        .cove_update(c.id.as_str(), CovePatch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn wave_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "first").await;
    assert!(w.archived_at.is_none());

    let updated = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: Some("renamed".into()),
                sort: None,
                archived_at: Some(Some(42)),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.title, "renamed");
    assert_eq!(updated.archived_at, Some(42));

    let cleared = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: None,
                sort: None,
                archived_at: Some(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.archived_at, None);

    let err = repo
        .wave_create(NewWave {
            cove_id: "no-such-cove".into(),
            title: "x".into(),
            sort: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn card_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    assert_eq!(card.payload, json!({"hello": "world"}));

    let updated = repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: Some("plugin:x:view".into()),
                sort: None,
                payload: Some(json!({"replaced": true})),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.kind, "plugin:x:view");
    assert_eq!(updated.payload, json!({"replaced": true}));

    let listed = repo.cards_by_wave(w.id.as_str()).await.unwrap();
    assert_eq!(listed.len(), 1);

    repo.card_delete(card.id.as_str()).await.unwrap();
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    let err = repo.card_delete(card.id.as_str()).await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

// ----------------------------------------------------------- Cascades ----

#[tokio::test]
async fn cove_delete_cascades_to_waves_and_cards() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w1 = make_wave(&repo, c.id.as_str(), "w1").await;
    let w2 = make_wave(&repo, c.id.as_str(), "w2").await;
    let c1 = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c2 = make_card(&repo, w2.id.as_str(), "terminal").await;

    repo.cove_delete(c.id.as_str()).await.unwrap();

    assert!(repo.wave_get(w1.id.as_str()).await.unwrap().is_none());
    assert!(repo.wave_get(w2.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(c1.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(c2.id.as_str()).await.unwrap().is_none());
}

#[tokio::test]
async fn wave_delete_cascades_to_cards() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let other_wave = make_wave(&repo, c.id.as_str(), "other").await;
    let other_card = make_card(&repo, other_wave.id.as_str(), "terminal").await;

    repo.wave_delete(w.id.as_str()).await.unwrap();

    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    // unrelated wave and card untouched
    assert!(
        repo.wave_get(other_wave.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        repo.card_get(other_card.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
}

// --- Cascade-to-terminals regression tests (issue #4 / D3) -----------------
//
// These three tests document the FK CASCADE contract: deleting a card, wave,
// or cove must remove every terminal beneath it. They pass for SqlxRepo
// (cascade is enforced by the schema) and would have failed for the old
// MockRepo, whose `card_delete` / `wave_delete` / `cove_delete` only walked
// the cove → wave → card layer and never reached the terminals map. Keep
// these around so a future "let's bring back the in-memory mock" attempt
// fails loudly at test time instead of silently leaving dangling terminals.

async fn make_terminal(repo: &SqlxRepo, card_id: &str) -> Terminal {
    repo.terminal_create(NewTerminal {
        card_id: card_id.into(),
        program: "bash".into(),
        cwd: "/tmp".into(),
        env: json!({}),
    })
    .await
    .expect("create terminal")
}

#[tokio::test]
async fn cascade_card_delete_removes_terminal() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    repo.card_delete(card.id.as_str()).await.unwrap();

    assert!(
        repo.terminal_get(term.id.as_str()).await.unwrap().is_none(),
        "terminal must cascade away when its parent card is deleted"
    );
}

#[tokio::test]
async fn cascade_wave_delete_removes_terminals() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    // Unrelated wave/card/terminal that must NOT be touched.
    let other_wave = make_wave(&repo, c.id.as_str(), "other").await;
    let other_card = make_card(&repo, other_wave.id.as_str(), "terminal").await;
    let other_term = make_terminal(&repo, other_card.id.as_str()).await;

    repo.wave_delete(w.id.as_str()).await.unwrap();

    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    assert!(
        repo.terminal_get(term.id.as_str()).await.unwrap().is_none(),
        "terminal must cascade away when its grand-parent wave is deleted"
    );
    // Sibling subtree intact.
    assert!(
        repo.wave_get(other_wave.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        repo.card_get(other_card.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        repo.terminal_get(other_term.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn cascade_cove_delete_removes_terminals() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    repo.cove_delete(c.id.as_str()).await.unwrap();

    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    assert!(
        repo.terminal_get(term.id.as_str()).await.unwrap().is_none(),
        "terminal must cascade away when its great-grand-parent cove is deleted"
    );
}

// ----------------------------------------------------- Sort defaulting ----

#[tokio::test]
async fn sort_defaulting_assigns_1_2_3_for_coves() {
    let repo = fresh_repo().await;
    let a = make_cove(&repo, "a").await;
    let b = make_cove(&repo, "b").await;
    let c = make_cove(&repo, "c").await;
    assert_eq!(a.sort, 1.0);
    assert_eq!(b.sort, 2.0);
    assert_eq!(c.sort, 3.0);
}

#[tokio::test]
async fn sort_defaulting_is_scoped_per_cove_for_waves() {
    let repo = fresh_repo().await;
    let c1 = make_cove(&repo, "c1").await;
    let c2 = make_cove(&repo, "c2").await;
    let w1a = make_wave(&repo, c1.id.as_str(), "w1a").await;
    let w1b = make_wave(&repo, c1.id.as_str(), "w1b").await;
    let w2a = make_wave(&repo, c2.id.as_str(), "w2a").await;
    assert_eq!(w1a.sort, 1.0);
    assert_eq!(w1b.sort, 2.0);
    // w2a is the first wave in c2 so it should also start at 1.0.
    assert_eq!(w2a.sort, 1.0);
}

#[tokio::test]
async fn sort_defaulting_is_scoped_per_wave_for_cards() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "c").await;
    let w1 = make_wave(&repo, c.id.as_str(), "w1").await;
    let w2 = make_wave(&repo, c.id.as_str(), "w2").await;
    let c1a = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c1b = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c1c = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c2a = make_card(&repo, w2.id.as_str(), "terminal").await;
    assert_eq!(c1a.sort, 1.0);
    assert_eq!(c1b.sort, 2.0);
    assert_eq!(c1c.sort, 3.0);
    assert_eq!(c2a.sort, 1.0);
}

// ------------------------------------------------------- wave_detail ----

#[tokio::test]
async fn wave_detail_includes_sorted_cards_and_scoped_overlays() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let other_w = make_wave(&repo, c.id.as_str(), "other").await;

    // Create cards in an out-of-order manner; expect sort = 1,2,3 sequential.
    let card_a = make_card(&repo, w.id.as_str(), "a").await;
    let card_b = make_card(&repo, w.id.as_str(), "b").await;
    let card_c = make_card(&repo, w.id.as_str(), "c").await;
    let other_card = make_card(&repo, other_w.id.as_str(), "other").await;

    // Overlays: one wave-scoped, one card-scoped (on card_b), and one on a
    // card in an unrelated wave (must be excluded).
    let wave_overlay = repo
        .overlay_upsert(NewOverlay {
            plugin_id: "p".into(),
            entity_kind: "wave".into(),
            entity_id: w.id.to_string(),
            kind: "status".into(),
            payload: json!({"state": "ok"}),
        })
        .await
        .unwrap();
    let card_overlay = repo
        .overlay_upsert(NewOverlay {
            plugin_id: "p".into(),
            entity_kind: "card".into(),
            entity_id: card_b.id.to_string(),
            kind: "badge".into(),
            payload: json!(7),
        })
        .await
        .unwrap();
    let _excluded = repo
        .overlay_upsert(NewOverlay {
            plugin_id: "p".into(),
            entity_kind: "card".into(),
            entity_id: other_card.id.to_string(),
            kind: "badge".into(),
            payload: json!("nope"),
        })
        .await
        .unwrap();

    let detail = repo
        .wave_detail(w.id.as_str())
        .await
        .unwrap()
        .expect("wave detail");
    assert_eq!(detail.wave.id, w.id);
    let card_ids: Vec<&str> = detail.cards.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        card_ids,
        vec![card_a.id.as_str(), card_b.id.as_str(), card_c.id.as_str()]
    );

    let overlay_ids: std::collections::HashSet<&str> =
        detail.overlays.iter().map(|o| o.id.as_str()).collect();
    assert!(overlay_ids.contains(wave_overlay.id.as_str()));
    assert!(overlay_ids.contains(card_overlay.id.as_str()));
    assert_eq!(detail.overlays.len(), 2);
}

#[tokio::test]
async fn wave_detail_returns_none_for_missing_wave() {
    let repo = fresh_repo().await;
    assert!(repo.wave_detail("nonexistent").await.unwrap().is_none());
}

// --------------------------------------------------------- overlays ----

#[tokio::test]
async fn overlay_upsert_is_idempotent_on_unique_key() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let p = NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "wave".into(),
        entity_id: w.id.to_string(),
        kind: "status".into(),
        payload: json!({"v": 1}),
    };
    let first = repo.overlay_upsert(p.clone()).await.unwrap();

    let mut p2 = p.clone();
    p2.payload = json!({"v": 2});
    let second = repo.overlay_upsert(p2).await.unwrap();

    // Same row (same id), updated payload.
    assert_eq!(first.id, second.id);
    assert_eq!(second.payload, json!({"v": 2}));

    let all = repo.overlays_for("wave", w.id.as_str()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].payload, json!({"v": 2}));

    repo.overlay_delete("p", "wave", w.id.as_str(), "status")
        .await
        .unwrap();
    let err = repo
        .overlay_delete("p", "wave", w.id.as_str(), "status")
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn overlays_by_kind_returns_all_wave_overlays_across_coves() {
    let repo = fresh_repo().await;
    let c1 = make_cove(&repo, "C1").await;
    let c2 = make_cove(&repo, "C2").await;
    let w1 = make_wave(&repo, c1.id.as_str(), "W1").await;
    let w2 = make_wave(&repo, c2.id.as_str(), "W2").await;
    let card = make_card(&repo, w1.id.as_str(), "terminal").await;

    // Two wave overlays in different coves + one card overlay.
    repo.overlay_upsert(NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "wave".into(),
        entity_id: w1.id.to_string(),
        kind: "status".into(),
        payload: json!({"state": "running"}),
    })
    .await
    .unwrap();
    repo.overlay_upsert(NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "wave".into(),
        entity_id: w2.id.to_string(),
        kind: "status".into(),
        payload: json!({"state": "waiting"}),
    })
    .await
    .unwrap();
    repo.overlay_upsert(NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "card".into(),
        entity_id: card.id.to_string(),
        kind: "status".into(),
        payload: json!({"state": "running"}),
    })
    .await
    .unwrap();

    let waves = repo.overlays_by_kind("wave").await.unwrap();
    assert_eq!(waves.len(), 2);
    let ids: std::collections::HashSet<&str> = waves.iter().map(|o| o.entity_id.as_str()).collect();
    assert!(ids.contains(w1.id.as_str()));
    assert!(ids.contains(w2.id.as_str()));
    assert!(waves.iter().all(|o| o.entity_kind == "wave"));

    let cards = repo.overlays_by_kind("card").await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].entity_id, card.id.as_str());
}

// --------------------------------------------------------- terminals ----

#[tokio::test]
async fn terminal_create_rejects_duplicate_card_id() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;

    let t = repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "bash".into(),
            cwd: "/tmp".into(),
            env: json!({"FOO": "bar"}),
        })
        .await
        .unwrap();
    assert!(t.daemon_handle.is_none());

    let err = repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "zsh".into(),
            cwd: "/tmp".into(),
            env: json!({}),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)));

    repo.terminal_set_handle(&t.id, Some("handle-1"))
        .await
        .unwrap();
    let got = repo.terminal_get(&t.id).await.unwrap().unwrap();
    assert_eq!(got.daemon_handle.as_deref(), Some("handle-1"));
    let by_card = repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_card.id, t.id);

    let err = repo.terminal_set_handle("no-such", None).await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));

    // Terminal cascades when its card is deleted.
    repo.card_delete(card.id.as_str()).await.unwrap();
    assert!(repo.terminal_get(&t.id).await.unwrap().is_none());
}

// ------------------------------------------- atomic terminal-card helpers ----
//
// Coverage for `terminal_create_tx` and `card_with_terminal_create_tx`, the
// new transactional helpers added for #13 PR1. These tests open transactions
// directly off the pool (like `write_with_event`'s closure does) to exercise
// the `_tx` surface without going through the pool-wrapping wrappers.

#[tokio::test]
async fn card_with_terminal_create_tx_atomic_writes_card_terminal_and_payload_link() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        w.id.clone(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({"FOO": "bar"}),
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .expect("atomic create");
    tx.commit().await.unwrap();

    // Card persisted with kind=terminal and the canonical payload link.
    let got_card = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    assert_eq!(got_card.kind, "terminal");
    assert_eq!(got_card.payload["terminal_id"], json!(term.id));
    assert_eq!(got_card.payload["schemaVersion"], json!(1));

    // Terminal persisted and parented to the card.
    let got_term = repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("terminal row");
    assert_eq!(got_term.id, term.id);
    assert_eq!(got_term.program, "bash");
    assert_eq!(got_term.cwd, "/tmp");
    assert_eq!(got_term.env, json!({"FOO": "bar"}));
}

#[tokio::test]
async fn card_with_terminal_create_tx_rolls_back_on_invalid_wave() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    // Sanity: wave has no cards yet, and no orphan terminals exist.
    assert!(repo.cards_by_wave(w.id.as_str()).await.unwrap().is_empty());

    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        "wave-that-does-not-exist".into(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .expect_err("unknown wave must error");
    // Explicit rollback so the txn doesn't linger; would be implicit on drop
    // but we make the intent visible.
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::NotFound(_)));

    // No card was left behind in the valid wave (it never had any), and no
    // terminal row exists at all — direct sqlx count against the table.
    let cards_in_w = repo.cards_by_wave(w.id.as_str()).await.unwrap();
    assert!(
        cards_in_w.is_empty(),
        "no card rows should have leaked from the rolled-back txn"
    );
    let term_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminals")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(term_count.0, 0, "no terminal rows should have been written");
}

#[tokio::test]
async fn card_with_terminal_create_tx_uses_caller_supplied_sort() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        w.id.clone(),
        Some(42.0),
        "bash".into(),
        "/tmp".into(),
        json!({}),
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(card.sort, 42.0);
    let got = repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.sort, 42.0);
}

#[tokio::test]
async fn card_with_terminal_create_tx_defaults_sort_when_none() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    // Pre-seed two cards so the next sort default lands at 3.0 — same
    // assertion shape as `sort_defaulting_is_scoped_per_wave_for_cards`.
    let _c1 = make_card(&repo, w.id.as_str(), "terminal").await;
    let _c2 = make_card(&repo, w.id.as_str(), "terminal").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        w.id.clone(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(card.sort, 3.0);
}

#[tokio::test]
async fn terminal_create_tx_enforces_unique_card_id() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let _seeded = make_terminal(&repo, card.id.as_str()).await;

    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::terminal_create_tx(
        &mut tx,
        NewTerminal {
            card_id: card.id.clone(),
            program: "zsh".into(),
            cwd: "/tmp".into(),
            env: json!({}),
        },
    )
    .await
    .expect_err("duplicate terminal for same card must conflict");
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::Conflict(_)));
}

#[tokio::test]
async fn terminal_create_tx_rejects_unknown_card_id() {
    let repo = fresh_repo().await;

    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::terminal_create_tx(
        &mut tx,
        NewTerminal {
            card_id: "no-such-card".into(),
            program: "bash".into(),
            cwd: "/tmp".into(),
            env: json!({}),
        },
    )
    .await
    .expect_err("unknown card must error");
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::NotFound(_)));
}

// -------------------------------------------- atomic codex-card helpers ----
//
// Coverage for `card_with_codex_create_tx`, the transactional helper added
// for #117. Mirrors the `card_with_terminal_create_tx` tests above — same
// pool().begin() pattern, same commit-before-assert / explicit-rollback
// shape. The codex helper takes a caller-supplied `card_id` (option C in
// the design doc), so the success-path tests pass `new_id()` from the
// public model module to keep id-collision realistic.

#[tokio::test]
async fn card_with_codex_create_tx_atomic_writes_card_terminal_and_payload_link() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        w.id.clone(),
        None,
        "/workspace".into(),
        json!({"CODEX_HOME": "/tmp/cx"}),
        None,
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .expect("atomic codex create");
    tx.commit().await.unwrap();

    assert_eq!(card.id.as_str(), card_id, "caller-supplied id must persist");
    let got_card = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    assert_eq!(got_card.kind, "codex");
    assert_eq!(got_card.payload["terminal_id"], json!(term.id));
    assert_eq!(got_card.payload["schemaVersion"], json!(1));
    // cwd is non-empty here — payload must carry it for the frontend's
    // status hint.
    assert_eq!(got_card.payload["cwd"], json!("/workspace"));

    let got_term = repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("terminal row");
    assert_eq!(got_term.id, term.id);
    assert_eq!(got_term.program, "codex");
    assert_eq!(got_term.cwd, "/workspace");
    assert_eq!(got_term.env, json!({"CODEX_HOME": "/tmp/cx"}));
}

#[tokio::test]
async fn card_with_codex_create_tx_rolls_back_on_invalid_wave() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    assert!(repo.cards_by_wave(w.id.as_str()).await.unwrap().is_empty());

    let card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id,
        "wave-that-does-not-exist".into(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .expect_err("unknown wave must error");
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::NotFound(_)));

    let cards_in_w = repo.cards_by_wave(w.id.as_str()).await.unwrap();
    assert!(
        cards_in_w.is_empty(),
        "no card rows should have leaked from the rolled-back txn"
    );
    let term_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminals")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(term_count.0, 0, "no terminal rows should have been written");
}

#[tokio::test]
async fn card_with_codex_create_tx_uses_caller_supplied_sort() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id,
        w.id.clone(),
        Some(7.0),
        "/workspace".into(),
        json!({}),
        None,
        &calm_server::card_role_cache::CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(card.sort, 7.0);
    let got = repo.card_get(card.id.as_str()).await.unwrap().unwrap();
    assert_eq!(got.sort, 7.0);
}

// ---------------------------------------------------------------- plugins ----

fn sample_new_plugin(id: &str, enabled: bool) -> NewPlugin {
    NewPlugin {
        id: id.into(),
        version: "0.1.0".into(),
        install_path: format!("/tmp/{id}"),
        manifest: json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "display_name": "Test",
        }),
        enabled,
        user_config: json!({}),
    }
}

#[tokio::test]
async fn plugin_install_get_list_round_trip() {
    let repo = fresh_repo().await;

    let p = repo
        .plugin_install(sample_new_plugin("p.one", false))
        .await
        .unwrap();
    assert_eq!(p.id, "p.one");
    assert!(!p.enabled);
    assert!(p.installed_at > 0);

    let got = repo
        .plugin_get_by_id("p.one")
        .await
        .unwrap()
        .expect("plugin exists");
    assert_eq!(got.version, "0.1.0");

    // Upsert keeps `installed_at`, bumps `updated_at`.
    let mut np = sample_new_plugin("p.one", true);
    np.version = "0.2.0".into();
    let p2 = repo.plugin_install(np).await.unwrap();
    assert_eq!(p2.installed_at, p.installed_at);
    assert!(p2.updated_at >= p.updated_at);
    assert!(p2.enabled);
    assert_eq!(p2.version, "0.2.0");

    repo.plugin_install(sample_new_plugin("p.two", false))
        .await
        .unwrap();
    let listed = repo.plugins_list_all().await.unwrap();
    assert_eq!(listed.len(), 2);

    let toggled = repo.plugin_update_enabled("p.two", true).await.unwrap();
    assert!(toggled.enabled);

    let err = repo
        .plugin_update_enabled("missing", true)
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));

    repo.plugin_delete("p.one").await.unwrap();
    assert!(repo.plugin_get_by_id("p.one").await.unwrap().is_none());
    let err = repo.plugin_delete("p.one").await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn plugin_token_round_trip() {
    let repo = fresh_repo().await;
    repo.plugin_install(sample_new_plugin("p.tok", false))
        .await
        .unwrap();

    assert!(repo.plugin_token_get("p.tok").await.unwrap().is_none());

    repo.plugin_token_set("p.tok", "hashed-v1", 1_000)
        .await
        .unwrap();
    let (h, exp) = repo.plugin_token_get("p.tok").await.unwrap().unwrap();
    assert_eq!(h, "hashed-v1");
    assert_eq!(exp, 1_000);

    // Rotate: overwrite via the same set call.
    repo.plugin_token_set("p.tok", "hashed-v2", 2_000)
        .await
        .unwrap();
    let (h, exp) = repo.plugin_token_get("p.tok").await.unwrap().unwrap();
    assert_eq!(h, "hashed-v2");
    assert_eq!(exp, 2_000);

    // Delete is idempotent.
    repo.plugin_token_delete("p.tok").await.unwrap();
    repo.plugin_token_delete("p.tok").await.unwrap();
    assert!(repo.plugin_token_get("p.tok").await.unwrap().is_none());
}

#[tokio::test]
async fn plugin_token_cascades_on_plugin_delete() {
    let repo = fresh_repo().await;
    repo.plugin_install(sample_new_plugin("p.casc", false))
        .await
        .unwrap();
    repo.plugin_token_set("p.casc", "h", 1).await.unwrap();
    repo.plugin_delete("p.casc").await.unwrap();
    assert!(repo.plugin_token_get("p.casc").await.unwrap().is_none());
}

#[tokio::test]
async fn plugin_kv_round_trip() {
    let repo = fresh_repo().await;
    repo.plugin_install(sample_new_plugin("p.kv", false))
        .await
        .unwrap();

    assert!(repo.plugin_kv_get("p.kv", "any").await.unwrap().is_none());

    repo.plugin_kv_set("p.kv", "run/1", &json!({"ok": true}))
        .await
        .unwrap();
    repo.plugin_kv_set("p.kv", "run/2", &json!(42))
        .await
        .unwrap();
    repo.plugin_kv_set("p.kv", "other", &json!("x"))
        .await
        .unwrap();

    let listed = repo.plugin_kv_list("p.kv", "run/").await.unwrap();
    let keys: Vec<&str> = listed.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["run/1", "run/2"]);
    assert_eq!(listed[1].1, json!(42));

    // Empty prefix lists everything for this plugin.
    let all = repo.plugin_kv_list("p.kv", "").await.unwrap();
    assert_eq!(all.len(), 3);

    // Other plugin's keys are not visible.
    repo.plugin_install(sample_new_plugin("p.other", false))
        .await
        .unwrap();
    repo.plugin_kv_set("p.other", "run/1", &json!("nope"))
        .await
        .unwrap();
    let listed = repo.plugin_kv_list("p.kv", "run/").await.unwrap();
    assert_eq!(listed.len(), 2);

    repo.plugin_kv_delete("p.kv", "run/1").await.unwrap();
    assert!(repo.plugin_kv_get("p.kv", "run/1").await.unwrap().is_none());
    // Idempotent.
    repo.plugin_kv_delete("p.kv", "run/1").await.unwrap();

    // Cascade on plugin_delete.
    repo.plugin_delete("p.kv").await.unwrap();
    assert!(repo.plugin_kv_list("p.kv", "").await.unwrap().is_empty());
}

#[tokio::test]
async fn plugin_kv_prefix_escapes_glob_chars() {
    // Prove the prefix isn't treated as a LIKE glob — `%` and `_` are literal.
    let repo = fresh_repo().await;
    repo.plugin_install(sample_new_plugin("p.glob", false))
        .await
        .unwrap();
    repo.plugin_kv_set("p.glob", "100%/a", &json!(1))
        .await
        .unwrap();
    repo.plugin_kv_set("p.glob", "100x/a", &json!(2))
        .await
        .unwrap();
    let listed = repo.plugin_kv_list("p.glob", "100%/").await.unwrap();
    let keys: Vec<&str> = listed.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["100%/a"]);
}

// ----- Upgrade stability: refuse-to-boot on unknown future migration --------
//
// `docs/upgrade-stability.md` (Tier A, DB schema): "old binary reading new
// DB → refuses boot with: 'database has migration X applied that this
// binary doesn't know about — refusing to boot; downgrade is not
// supported'". `SqlxRepo::open` enforces this before the embedded migrator
// gets to apply anything.

/// Simulate an "older binary reading newer DB": open a fresh repo (which
/// migrates the schema to the binary's current set), inject a synthetic
/// future-version row into `_sqlx_migrations`, then reopen and assert the
/// open is rejected.
///
/// Uses an on-disk tempfile so the second `SqlxRepo::open` actually
/// observes the row we wrote — `sqlite::memory:` would give us a fresh DB
/// the second time around.
#[tokio::test]
async fn open_refuses_unknown_future_migration() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let url = format!("sqlite://{}?mode=rwc", tmp.path().display());

    // First open: runs migrations to current; `_sqlx_migrations` now exists
    // and contains rows 0001..=0005 (all known versions).
    {
        let repo = SqlxRepo::open(&url).await.expect("initial open");
        // Inject a synthetic future migration row. sqlx's expected schema:
        // (version, description, installed_on, success, checksum, execution_time).
        // The values are arbitrary — only `version` matters for the guard.
        sqlx::query(
            r#"INSERT INTO _sqlx_migrations
                   (version, description, installed_on, success, checksum, execution_time)
               VALUES (?1, ?2, CURRENT_TIMESTAMP, 1, ?3, 0)"#,
        )
        .bind(99_999_999_i64)
        .bind("synthetic future migration")
        .bind(b"\0\0\0\0".as_slice())
        .execute(repo.pool())
        .await
        .expect("insert synthetic future migration row");
        // Drop `repo` so its pool releases the file lock before reopen.
    }

    // Second open: must refuse with the typed error + agreed wording.
    // `SqlxRepo` isn't `Debug`, so `expect_err` is unavailable — match.
    let err = match SqlxRepo::open(&url).await {
        Ok(_) => panic!("reopen must refuse on unknown future migration"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        matches!(err, CalmError::Internal(_)),
        "expected CalmError::Internal, got: {err:?}",
    );
    assert!(
        msg.contains("99999999"),
        "error message should name the unknown version 99999999: {msg}",
    );
    assert!(
        msg.contains("refusing to boot"),
        "error message should contain 'refusing to boot': {msg}",
    );
    assert!(
        msg.contains("downgrade is not supported"),
        "error message should contain 'downgrade is not supported': {msg}",
    );
    assert!(
        msg.contains("doesn't know about"),
        "error message should contain 'doesn't know about': {msg}",
    );
}

/// Brand-new DB (no `_sqlx_migrations` row yet) and "current binary on
/// current DB" both open cleanly. Belt-and-braces against a regression
/// where the guard would mis-flag a known applied version, or fail when
/// the table doesn't exist yet.
#[tokio::test]
async fn open_succeeds_on_fresh_and_current_db() {
    // Fresh in-memory DB: `_sqlx_migrations` doesn't exist before the
    // migrator's first `run()`. The guard must tolerate that.
    let _ = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("fresh in-memory open succeeds");

    // Tempfile DB, opened twice: the second open sees all known versions
    // already applied and must still succeed.
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let url = format!("sqlite://{}?mode=rwc", tmp.path().display());
    let _ = SqlxRepo::open(&url).await.expect("first open");
    let _ = SqlxRepo::open(&url)
        .await
        .expect("reopen with current binary");
}
