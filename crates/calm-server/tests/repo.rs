//! Integration tests for `SqlxRepo` against an in-memory SQLite.
//!
//! These tests exercise the observable contract of the `Repo` trait against
//! the real sqlx-backed implementation: CRUD round-trips, cascade deletes,
//! sort defaulting, `wave_detail` composition, overlay upsert idempotency,
//! and terminal-per-card uniqueness.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, overlay_delete_by_entity_tx, session_prepare_deferred_spec_tx,
    session_start_runtime_tx,
};
use calm_server::error::CalmError;
use calm_server::model::*;
use calm_server::session_projection_lookup::project_runtime_into_card_payload;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
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
        cwd: String::new(),
        workflow_id: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
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

fn runtime_init(
    card_id: String,
    kind: WorkerSessionKind,
    agent_provider: Option<AgentProvider>,
) -> WorkerSessionInit {
    WorkerSessionInit {
        id: new_id(),
        card_id,
        kind,
        agent_provider,
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    }
}

async fn start_root_runtime(repo: &SqlxRepo, card: &Card) -> String {
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    let runtime = session_start_runtime_tx(
        &mut tx,
        runtime_init(
            card.id.to_string(),
            WorkerSessionKind::SharedSpec,
            Some(AgentProvider::Codex),
        ),
    )
    .await
    .expect("start root runtime");
    tx.commit().await.expect("commit runtime tx");
    runtime.id
}

async fn make_overlay(
    repo: &SqlxRepo,
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
) -> Overlay {
    repo.overlay_upsert(NewOverlay {
        plugin_id: plugin_id.into(),
        entity_kind: entity_kind.into(),
        entity_id: entity_id.into(),
        kind: kind.into(),
        payload: json!({"schemaVersion": 1, "state": "idle"}),
    })
    .await
    .expect("upsert overlay")
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
    // Issue #145 — every newly minted wave seeds at Draft.
    assert_eq!(
        w.lifecycle,
        WaveLifecycle::Draft,
        "new wave defaults to Draft"
    );

    let updated = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: Some("renamed".into()),
                sort: None,
                archived_at: Some(Some(42)),
                pinned_at: None,
                lifecycle: None,
                ..WavePatch::default()
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
                pinned_at: None,
                lifecycle: None,
                ..WavePatch::default()
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
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn wave_lifecycle_round_trips_through_patch() {
    // Issue #145 — `WavePatch.lifecycle` writes the column and the
    // next read reflects the new value. The validator (whose job is
    // to refuse illegal transitions) lives one layer up in the
    // routes / MCP tool; the DB layer accepts any value and is the
    // mechanical actuator. This test pins the read/write round-trip
    // so a future refactor that drops the column from the UPDATE
    // statement surfaces here.
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "lifecycle-test").await;
    assert_eq!(w.lifecycle, WaveLifecycle::Draft);

    let patched = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: None,
                sort: None,
                archived_at: None,
                pinned_at: None,
                lifecycle: Some(WaveLifecycle::Planning),
                ..WavePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.lifecycle, WaveLifecycle::Planning);

    let re_read = repo.wave_get(w.id.as_str()).await.unwrap().unwrap();
    assert_eq!(re_read.lifecycle, WaveLifecycle::Planning);

    // Patch with `lifecycle: None` leaves the column alone.
    let no_change = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: Some("renamed-only".into()),
                sort: None,
                archived_at: None,
                pinned_at: None,
                lifecycle: None,
                ..WavePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(no_change.lifecycle, WaveLifecycle::Planning);
}

#[tokio::test]
async fn events_for_wave_filters_since_in_query() {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::event::{Event, EventBus, EventScope};
    use calm_server::ids::ActorId;
    use calm_server::wave_cove_cache::WaveCoveCache;

    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let wave = make_wave(&repo, c.id.as_str(), "events-wave").await;
    let other_wave = make_wave(&repo, c.id.as_str(), "other-wave").await;
    let bus = EventBus::new();
    let role_cache = CardRoleCache::new();
    let cove_cache = WaveCoveCache::new();
    repo.seed_card_role_cache(&role_cache).await.unwrap();
    repo.seed_wave_cove_cache(&cove_cache).await.unwrap();

    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: c.id.clone(),
    };
    let other_scope = EventScope::Wave {
        wave: other_wave.id.clone(),
        cove: c.id.clone(),
    };
    let first_id = repo
        .log_pure_event(
            ActorId::Kernel,
            scope.clone(),
            None,
            &bus,
            &role_cache,
            &cove_cache,
            Event::TaskFailed {
                idempotency_key: "before-watermark".into(),
                reason: "before".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    repo.log_pure_event(
        ActorId::Kernel,
        other_scope,
        None,
        &bus,
        &role_cache,
        &cove_cache,
        Event::TaskFailed {
            idempotency_key: "other-wave".into(),
            reason: "other".into(),
            agent_message: None,
        },
    )
    .await
    .unwrap();
    let second_id = repo
        .log_pure_event(
            ActorId::Kernel,
            scope,
            None,
            &bus,
            &role_cache,
            &cove_cache,
            Event::TaskFailed {
                idempotency_key: "after-watermark".into(),
                reason: "after".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();

    let all = repo
        .events_for_wave(wave.id.as_str(), &["task.failed"], None)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![first_id, second_id],
        "unbounded wave query should include both matching events for the wave"
    );

    let since_first = repo
        .events_for_wave(wave.id.as_str(), &["task.failed"], Some(first_id))
        .await
        .unwrap();
    assert_eq!(
        since_first.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![second_id],
        "bounded wave query should apply id > watermark before returning rows"
    );
    assert_eq!(since_first[0].actor, ActorId::Kernel);

    let since_second = repo
        .events_for_wave(wave.id.as_str(), &["task.failed"], Some(second_id))
        .await
        .unwrap();
    assert!(since_second.is_empty());
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
                deletable: None,
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
async fn cove_delete_succeeds_when_wave_references_root_session() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo, "rooted").await;
    let wave = make_wave(&repo, cove.id.as_str(), "rooted wave").await;
    let root_card = make_card(&repo, wave.id.as_str(), "codex").await;
    let root_session_id = start_root_runtime(&repo, &root_card).await;

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(root_session_id.as_str()));

    repo.cove_delete(cove.id.as_str()).await.unwrap();

    assert!(repo.cove_get(cove.id.as_str()).await.unwrap().is_none());
    assert!(repo.wave_get(wave.id.as_str()).await.unwrap().is_none());
    assert!(
        repo.card_get(root_card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
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

#[tokio::test]
async fn root_card_delete_clears_wave_root_session_id() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo, "rooted-card").await;
    let wave = make_wave(&repo, cove.id.as_str(), "rooted wave").await;
    let root_card = make_card(&repo, wave.id.as_str(), "codex").await;
    let other_card = make_card(&repo, wave.id.as_str(), "terminal").await;
    let root_session_id = start_root_runtime(&repo, &root_card).await;

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(root_session_id.as_str()));

    repo.card_delete(root_card.id.as_str()).await.unwrap();

    assert!(
        repo.card_get(root_card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.card_get(other_card.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        root, None,
        "deleting the root card must detach the wave root"
    );
}

#[tokio::test]
async fn card_delete_sweeps_card_overlays() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;

    make_overlay(&repo, "p1", "card", card.id.as_str(), "status").await;
    make_overlay(&repo, "p2", "card", card.id.as_str(), "badge").await;

    repo.card_delete(card.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("card", card.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn wave_delete_sweeps_card_overlays() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card1 = make_card(&repo, w.id.as_str(), "terminal").await;
    let card2 = make_card(&repo, w.id.as_str(), "terminal").await;

    make_overlay(&repo, "p", "card", card1.id.as_str(), "status").await;
    make_overlay(&repo, "p", "card", card2.id.as_str(), "status").await;

    repo.wave_delete(w.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("card", card1.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.overlays_for("card", card2.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn wave_delete_sweeps_wave_and_view_overlays() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    make_overlay(&repo, "p", "wave", w.id.as_str(), "status").await;
    make_overlay(&repo, "p", "view", w.id.as_str(), "status").await;

    repo.wave_delete(w.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("wave", w.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.overlays_for("view", w.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cove_delete_sweeps_all_overlays_transitively() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    make_overlay(&repo, "p", "cove", c.id.as_str(), "status").await;

    let waves = [
        make_wave(&repo, c.id.as_str(), "w1").await,
        make_wave(&repo, c.id.as_str(), "w2").await,
    ];
    let mut card_ids: Vec<String> = Vec::new();

    for wave in &waves {
        make_overlay(&repo, "p", "wave", wave.id.as_str(), "status").await;
        make_overlay(&repo, "p", "view", wave.id.as_str(), "status").await;

        for name in ["c1", "c2"] {
            let card = make_card(&repo, wave.id.as_str(), name).await;
            make_overlay(&repo, "p", "card", card.id.as_str(), "status").await;
            card_ids.push(card.id.to_string());
        }
    }

    repo.cove_delete(c.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("cove", c.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    for wave in &waves {
        assert!(
            repo.overlays_for("wave", wave.id.as_str())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.overlays_for("view", wave.id.as_str())
                .await
                .unwrap()
                .is_empty()
        );
    }
    for card_id in &card_ids {
        assert!(repo.overlays_for("card", card_id).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn overlay_sweep_is_idempotent_no_rows() {
    let repo = fresh_repo().await;
    let mut tx = repo.pool().begin().await.unwrap();

    let rows = overlay_delete_by_entity_tx(&mut tx, "card", "missing-card")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(rows, 0);
}

// --- Terminal FK contract regression tests (issues #4, #197) ---------------
//
// Originally these three tests documented the `ON DELETE CASCADE` FK on
// `terminals.card_id`: deleting a card / wave / cove silently nuked the
// terminal row beneath it. Issue #197 inverted that contract: the FK is now
// `ON DELETE RESTRICT` (migration 0011) so the schema **refuses** to nuke
// the terminal row implicitly — eager teardown in the route handlers
// (`routes/cards.rs::delete_card`, `routes/waves.rs::delete_wave`,
// `routes/coves.rs::delete_cove`) owns the kill-daemon-unlink-socket
// sequence and explicitly drops the terminal row before the parent.
//
// The tests below now verify the RESTRICT semantics at the bare
// `Repo::card_delete` / `wave_delete` / `cove_delete` surface: a card/
// wave/cove that has a live terminal underneath cannot be deleted; once
// the terminal row is removed, the parent delete proceeds.

async fn make_terminal(repo: &SqlxRepo, card_id: &str) -> Terminal {
    repo.terminal_create(NewTerminal {
        card_id: card_id.into(),
        program: "bash".into(),
        cwd: "/tmp".into(),
        env: json!({}),
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create terminal")
}

#[tokio::test]
async fn fk_restrict_card_delete_blocked_by_terminal() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    // RESTRICT bites: the terminal row's `card_id` still points at the
    // card, so the schema refuses the parent delete.
    let err = repo.card_delete(card.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "expected an FK constraint error from sqlx, got: {err:?}"
    );
    // Terminal + card both intact.
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_some());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_some());

    // Eager-teardown shape: drop the terminal first, then the card.
    repo.terminal_delete(term.id.as_str()).await.unwrap();
    repo.card_delete(card.id.as_str()).await.unwrap();
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_none());
}

#[tokio::test]
async fn fk_restrict_wave_delete_blocked_by_terminal_under_card() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    // Unrelated wave/card/terminal that must NOT be touched on either
    // attempt (the second attempt succeeds, but only on `w`'s subtree).
    let other_wave = make_wave(&repo, c.id.as_str(), "other").await;
    let other_card = make_card(&repo, other_wave.id.as_str(), "terminal").await;
    let other_term = make_terminal(&repo, other_card.id.as_str()).await;

    // RESTRICT bites: the wave-delete cascade through `cards.wave_id`
    // would try to delete `card`, which still has `term` pointing at
    // it — schema refuses.
    let err = repo.wave_delete(w.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "expected an FK constraint error from sqlx, got: {err:?}"
    );
    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_some());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_some());
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_some());

    // Drain the terminal first (the eager-teardown shape), then the
    // wave delete clears the rest via CASCADE on `cards.wave_id`.
    repo.terminal_delete(term.id.as_str()).await.unwrap();
    repo.wave_delete(w.id.as_str()).await.unwrap();
    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());

    // Sibling subtree intact across both attempts.
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
async fn fk_restrict_cove_delete_blocked_by_terminal_under_subtree() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    let err = repo.cove_delete(c.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "expected an FK constraint error from sqlx, got: {err:?}"
    );
    assert!(repo.cove_get(c.id.as_str()).await.unwrap().is_some());
    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_some());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_some());
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_some());

    repo.terminal_delete(term.id.as_str()).await.unwrap();
    repo.cove_delete(c.id.as_str()).await.unwrap();
    assert!(repo.cove_get(c.id.as_str()).await.unwrap().is_none());
    assert!(repo.wave_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let err = repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "zsh".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)));

    let by_card = repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_card.id, t.id);

    // Issue #197 — `terminals.card_id` is `ON DELETE RESTRICT` so the
    // schema refuses a card delete that would orphan the terminal row.
    // Eager-teardown shape: drop the terminal first.
    let err = repo.card_delete(card.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "card delete with live terminal must fail with an FK error, got: {err:?}"
    );
    repo.terminal_delete(t.id.as_str()).await.unwrap();
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
async fn card_with_terminal_create_tx_atomic_writes_card_terminal_and_runtime() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({"FOO": "bar"}),
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("atomic create");
    tx.commit().await.unwrap();

    // Card persisted with kind=terminal and schema payload only; identity
    // lives in runtimes and is projected at read time.
    let got_card = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    assert_eq!(got_card.kind, "terminal");
    assert!(
        got_card.payload.get("terminal_id").is_none(),
        "terminal_id must not be persisted in cards.payload: {}",
        got_card.payload
    );
    assert_eq!(got_card.payload["schemaVersion"], json!(1));
    let runtime = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.terminal_run_id.as_deref(), Some(term.id.as_str()));
    let mut projected = got_card.clone();
    project_runtime_into_card_payload(&repo, &mut projected)
        .await
        .unwrap();
    assert_eq!(projected.payload["terminal_id"], json!(term.id));

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
        &calm_server::model::new_id(),
        None,
        "wave-that-does-not-exist".into(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
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
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        Some(42.0),
        "bash".into(),
        "/tmp".into(),
        json!({}),
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
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
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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
async fn card_with_codex_create_tx_atomic_writes_card_terminal_and_runtime() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;

    let card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    // PR7a (#136) — third tuple slot is the raw per-card MCP token;
    // Worker codex cards mint one so user-facing agents can call MCP.
    let (card, term, mcp_token) = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        None,
        "/workspace".into(),
        json!({"CODEX_HOME": "/tmp/cx"}),
        None,
        Some("#111111".into()),
        Some("#ffffff".into()),
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("atomic codex create");
    tx.commit().await.unwrap();

    assert!(
        mcp_token.is_some(),
        "Worker codex cards must mint an MCP token"
    );
    assert_eq!(card.id.as_str(), card_id, "caller-supplied id must persist");
    let got_card = repo
        .card_get(card.id.as_str())
        .await
        .unwrap()
        .expect("card row");
    assert_eq!(got_card.kind, "codex");
    assert!(
        got_card.payload.get("terminal_id").is_none(),
        "terminal_id must not be persisted in cards.payload: {}",
        got_card.payload
    );
    assert_eq!(got_card.payload["schemaVersion"], json!(1));
    assert_eq!(got_card.payload["icon_bg"], json!("#111111"));
    assert_eq!(got_card.payload["icon_fg"], json!("#ffffff"));
    // cwd is non-empty here — payload must carry it for the frontend's
    // status hint.
    assert_eq!(got_card.payload["cwd"], json!("/workspace"));
    let runtime = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.terminal_run_id.as_deref(), Some(term.id.as_str()));
    let mut projected = got_card.clone();
    project_runtime_into_card_payload(&repo, &mut projected)
        .await
        .unwrap();
    assert_eq!(projected.payload["terminal_id"], json!(term.id));

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
        &calm_server::model::new_id(),
        None,
        "wave-that-does-not-exist".into(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
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
    // PR7a (#136) — third tuple slot is the raw per-card MCP token;
    // unused here.
    let (card, _term, _mcp_token) = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id,
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        Some(7.0),
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        calm_server::model::CardRole::Worker,
        true,
        &calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
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
    let err: CalmError = match SqlxRepo::open(&url).await {
        Ok(_) => panic!("reopen must refuse on unknown future migration"),
        Err(e) => e.into(),
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

// ---------------------------------------------- #306 terminal_set_exit ----

/// Round-trip every branch of `terminal_set_exit` so the SQL writes both
/// columns coherently and the read path surfaces them via
/// `Terminal.exit_code` + `signal_killed`. The four states correspond to
/// the four shapes the daemon can write to `<sock>.exit`:
///
///   - clean exit (`exit_code = Some(0)`)
///   - non-zero exit (`exit_code = Some(137)`)
///   - signal-killed (`exit_code = None`, `signal_killed = true`)
///   - back to unset (`exit_code = None`, `signal_killed = false`) —
///     not a real daemon write path, but exercised here so a future
///     "clear exit on respawn" caller has a known-good shape.
#[tokio::test]
async fn terminal_set_exit_round_trip_all_branches() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let t = repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "bash".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Fresh row → both fields default per the 0020 migration:
    //   exit_code IS NULL, signal_killed = 0.
    assert_eq!(t.exit_code, None);
    assert!(!t.signal_killed);

    // (a) clean exit
    repo.terminal_set_exit(&t.id, Some(0), false).await.unwrap();
    let r = repo.terminal_get(&t.id).await.unwrap().unwrap();
    assert_eq!(r.exit_code, Some(0));
    assert!(!r.signal_killed);

    // (b) non-zero exit
    repo.terminal_set_exit(&t.id, Some(137), false)
        .await
        .unwrap();
    let r = repo.terminal_get(&t.id).await.unwrap().unwrap();
    assert_eq!(r.exit_code, Some(137));
    assert!(!r.signal_killed);

    // (c) signal-killed (mutually exclusive: exit_code = None)
    repo.terminal_set_exit(&t.id, None, true).await.unwrap();
    let r = repo.terminal_get(&t.id).await.unwrap().unwrap();
    assert_eq!(r.exit_code, None);
    assert!(r.signal_killed);

    // (d) clear back to unset
    repo.terminal_set_exit(&t.id, None, false).await.unwrap();
    let r = repo.terminal_get(&t.id).await.unwrap().unwrap();
    assert_eq!(r.exit_code, None);
    assert!(!r.signal_killed);

    // Missing id → NotFound, mirroring `terminal_set_pid`.
    let err = repo
        .terminal_set_exit("no-such-id", Some(0), false)
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn shared_initial_prompt_takeover_returns_live_pending_shared_specs() {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::model::{CardRole, NewCard};

    let repo = fresh_repo().await;
    let c = make_cove(&repo, "shared-boot-exclusion").await;
    let mapped_wave = make_wave(&repo, c.id.as_str(), "mapped").await;
    let pending_wave = make_wave(&repo, c.id.as_str(), "").await;
    let phantom_wave = make_wave(&repo, c.id.as_str(), "phantom").await;
    let cache = CardRoleCache::new();

    let pending_card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let mapped = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            wave_id: mapped_wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "codex_source": "shared",
                "codex_thread_id": "T-shared-mapped",
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Spec,
        false,
        &cache,
    )
    .await
    .expect("create mapped shared spec card");
    let pending = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        pending_card_id.clone(),
        NewCard {
            wave_id: pending_wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Spec,
        false,
        &cache,
    )
    .await
    .expect("create pending shared spec card");
    let phantom = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            wave_id: phantom_wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Spec,
        false,
        &cache,
    )
    .await
    .expect("create deferred placeholder shared spec card");
    tx.commit().await.unwrap();

    // Shared takeover now keys off an active shared-spec runtime pointing
    // at a live terminal, not payload identity stamps.
    let mapped_term = make_terminal(&repo, mapped.id.as_str()).await;
    let term = make_terminal(&repo, pending.id.as_str()).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: calm_server::model::new_id(),
            card_id: mapped.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: Some(mapped_term.id.to_string()),
            thread_id: Some("T-shared-mapped".to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: calm_server::model::new_id(),
            card_id: pending.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some(term.id.to_string()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    let phantom_session_id = calm_server::model::new_id();
    session_prepare_deferred_spec_tx(
        &mut tx,
        &WorkerSessionInit {
            id: phantom_session_id.clone(),
            card_id: phantom.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(json!({"mode": "harness"})),
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let phantom_mirror: Option<String> =
        sqlx::query_scalar("SELECT id FROM worker_sessions WHERE id = ?1")
            .bind(&phantom_session_id)
            .fetch_optional(repo.pool())
            .await
            .unwrap();
    assert_eq!(phantom_mirror.as_deref(), Some(phantom_session_id.as_str()));

    assert_eq!(
        repo.shared_spec_cards_for_initial_prompt_takeover()
            .await
            .expect("shared pending takeover query"),
        vec![(
            pending.id.to_string(),
            pending_wave.id.to_string(),
            term.id.to_string(),
            0,
        )]
    );

    // Marking the terminal exited removes the card from the takeover set
    // (R7 P2 #1) — dead-TUI cards must not be re-registered into the FIFO.
    repo.terminal_set_exit(term.id.as_str(), Some(0), false)
        .await
        .unwrap();
    assert!(
        repo.shared_spec_cards_for_initial_prompt_takeover()
            .await
            .expect("shared pending takeover query after terminal exit")
            .is_empty(),
        "exited terminal must drop the card from shared pending takeover"
    );
}
