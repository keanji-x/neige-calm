//! Integration tests for `SqlxRepo` against an in-memory SQLite.
//!
//! These tests exercise the observable contract of the `Repo` trait against
//! the real sqlx-backed implementation: CRUD round-trips, cascade deletes,
//! sort defaulting, `track_detail` composition, overlay upsert idempotency,
//! and terminal-per-card uniqueness.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, overlay_delete_by_entity_tx, session_prepare_deferred_planner_tx,
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

async fn make_area(repo: &SqlxRepo, name: &str) -> Area {
    repo.area_create(NewArea {
        name: name.into(),
        color: "#abcdef".into(),
        sort: None,
    })
    .await
    .expect("create area")
}

async fn make_track(repo: &SqlxRepo, area_id: &str, title: &str) -> Track {
    repo.track_create(NewTrack {
        template_input: None,
        area_id: area_id.into(),
        title: title.into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create track")
}

async fn make_card(repo: &SqlxRepo, track_id: &str, kind: &str) -> Card {
    repo.card_create(NewCard {
        track_id: track_id.into(),
        title: None,
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
            WorkerSessionKind::SharedPlanner,
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
async fn area_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "Personal").await;
    assert_eq!(c.name, "Personal");

    let got = repo
        .area_get(c.id.as_str())
        .await
        .unwrap()
        .expect("area exists");
    assert_eq!(got.id, c.id);

    let listed = repo.areas_list().await.unwrap();
    assert_eq!(listed.len(), 1);

    let updated = repo
        .area_update(
            c.id.as_str(),
            AreaPatch {
                name: Some("Work".into()),
                color: None,
                sort: None,
                default_template_id: None,
                default_cwd: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Work");
    assert_eq!(updated.color, c.color);

    repo.area_delete(c.id.as_str()).await.unwrap();
    assert!(repo.area_get(c.id.as_str()).await.unwrap().is_none());

    let err = repo.area_delete(c.id.as_str()).await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
    let err = repo
        .area_update(c.id.as_str(), AreaPatch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn area_updates_advance_timestamp_strictly() {
    let repo = fresh_repo().await;
    let area = make_area(&repo, "Original").await;
    let future = now_ms() + 60_000;
    sqlx::query("UPDATE areas SET updated_at = ?1 WHERE id = ?2")
        .bind(future)
        .bind(area.id.as_str())
        .execute(repo.pool())
        .await
        .expect("seed future Area timestamp");

    let first = repo
        .area_update(
            area.id.as_str(),
            AreaPatch {
                name: Some("First".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .expect("first Area update");
    assert_eq!(first.updated_at, future + 1);

    let second = repo
        .area_update(
            area.id.as_str(),
            AreaPatch {
                name: Some("Second".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .expect("second Area update");
    assert!(second.updated_at > first.updated_at);
}

#[tokio::test]
async fn track_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "first").await;
    assert!(w.archived_at.is_none());
    // Issue #145 — every newly minted track seeds at Draft.
    assert_eq!(
        w.lifecycle,
        TrackLifecycle::Draft,
        "new track defaults to Draft"
    );

    let updated = repo
        .track_update(
            w.id.as_str(),
            TrackPatch {
                title: Some("renamed".into()),
                sort: None,
                archived_at: Some(Some(42)),
                pinned_at: None,
                lifecycle: None,
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.title, "renamed");
    assert_eq!(updated.archived_at, Some(42));

    let cleared = repo
        .track_update(
            w.id.as_str(),
            TrackPatch {
                title: None,
                sort: None,
                archived_at: Some(None),
                pinned_at: None,
                lifecycle: None,
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.archived_at, None);

    let err = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: "no-such-area".into(),
            title: "x".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn track_lifecycle_round_trips_through_patch() {
    // Issue #145 — `TrackPatch.lifecycle` writes the column and the
    // next read reflects the new value. The validator (whose job is
    // to refuse illegal transitions) lives one layer up in the
    // routes / MCP tool; the DB layer accepts any value and is the
    // mechanical actuator. This test pins the read/write round-trip
    // so a future refactor that drops the column from the UPDATE
    // statement surfaces here.
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "lifecycle-test").await;
    assert_eq!(w.lifecycle, TrackLifecycle::Draft);

    let patched = repo
        .track_update(
            w.id.as_str(),
            TrackPatch {
                title: None,
                sort: None,
                archived_at: None,
                pinned_at: None,
                lifecycle: Some(TrackLifecycle::Planning),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.lifecycle, TrackLifecycle::Planning);

    let re_read = repo.track_get(w.id.as_str()).await.unwrap().unwrap();
    assert_eq!(re_read.lifecycle, TrackLifecycle::Planning);

    // Patch with `lifecycle: None` leaves the column alone.
    let no_change = repo
        .track_update(
            w.id.as_str(),
            TrackPatch {
                title: Some("renamed-only".into()),
                sort: None,
                archived_at: None,
                pinned_at: None,
                lifecycle: None,
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(no_change.lifecycle, TrackLifecycle::Planning);
}

#[tokio::test]
async fn events_for_track_filters_since_in_query() {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::event::{Event, EventBus, EventScope};
    use calm_server::ids::ActorId;
    use calm_server::track_area_cache::TrackAreaCache;

    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let track = make_track(&repo, c.id.as_str(), "events-track").await;
    let other_track = make_track(&repo, c.id.as_str(), "other-track").await;
    let bus = EventBus::new();
    let role_cache = CardRoleCache::new();
    let area_cache = TrackAreaCache::new();
    repo.seed_card_role_cache(&role_cache).await.unwrap();
    repo.seed_track_area_cache(&area_cache).await.unwrap();

    let scope = EventScope::Track {
        track: track.id.clone(),
        area: c.id.clone(),
    };
    let other_scope = EventScope::Track {
        track: other_track.id.clone(),
        area: c.id.clone(),
    };
    let first_id = repo
        .log_pure_event(
            ActorId::Kernel,
            scope.clone(),
            None,
            &bus,
            &role_cache,
            &area_cache,
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
        &area_cache,
        Event::TaskFailed {
            idempotency_key: "other-track".into(),
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
            &area_cache,
            Event::TaskFailed {
                idempotency_key: "after-watermark".into(),
                reason: "after".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();

    let all = repo
        .events_for_track(track.id.as_str(), &["task.failed"], None)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![first_id, second_id],
        "unbounded track query should include both matching events for the track"
    );

    let since_first = repo
        .events_for_track(track.id.as_str(), &["task.failed"], Some(first_id))
        .await
        .unwrap();
    assert_eq!(
        since_first.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![second_id],
        "bounded track query should apply id > watermark before returning rows"
    );
    assert_eq!(since_first[0].actor, ActorId::Kernel);

    let since_second = repo
        .events_for_track(track.id.as_str(), &["task.failed"], Some(second_id))
        .await
        .unwrap();
    assert!(since_second.is_empty());
}

#[tokio::test]
async fn card_crud_round_trip() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    assert_eq!(card.payload, json!({"hello": "world"}));

    let updated = repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                title: None,
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

    let listed = repo.cards_by_track(w.id.as_str()).await.unwrap();
    assert_eq!(listed.len(), 1);

    repo.card_delete(card.id.as_str()).await.unwrap();
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    let err = repo.card_delete(card.id.as_str()).await.unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

// ----------------------------------------------------------- Cascades ----

#[tokio::test]
async fn area_delete_cascades_to_tracks_and_cards() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w1 = make_track(&repo, c.id.as_str(), "w1").await;
    let w2 = make_track(&repo, c.id.as_str(), "w2").await;
    let c1 = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c2 = make_card(&repo, w2.id.as_str(), "terminal").await;

    repo.area_delete(c.id.as_str()).await.unwrap();

    assert!(repo.track_get(w1.id.as_str()).await.unwrap().is_none());
    assert!(repo.track_get(w2.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(c1.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(c2.id.as_str()).await.unwrap().is_none());
}

#[tokio::test]
async fn area_delete_succeeds_when_track_references_root_session() {
    let repo = fresh_repo().await;
    let area = make_area(&repo, "rooted").await;
    let track = make_track(&repo, area.id.as_str(), "rooted track").await;
    let root_card = make_card(&repo, track.id.as_str(), "codex").await;
    let root_session_id = start_root_runtime(&repo, &root_card).await;

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(root.as_deref(), Some(root_session_id.as_str()));

    repo.area_delete(area.id.as_str()).await.unwrap();

    assert!(repo.area_get(area.id.as_str()).await.unwrap().is_none());
    assert!(repo.track_get(track.id.as_str()).await.unwrap().is_none());
    assert!(
        repo.card_get(root_card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn track_delete_cascades_to_cards() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let other_track = make_track(&repo, c.id.as_str(), "other").await;
    let other_card = make_card(&repo, other_track.id.as_str(), "terminal").await;

    repo.track_delete(w.id.as_str()).await.unwrap();

    assert!(repo.track_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
    // unrelated track and card untouched
    assert!(
        repo.track_get(other_track.id.as_str())
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
async fn root_card_delete_clears_track_root_session_id() {
    let repo = fresh_repo().await;
    let area = make_area(&repo, "rooted-card").await;
    let track = make_track(&repo, area.id.as_str(), "rooted track").await;
    let root_card = make_card(&repo, track.id.as_str(), "codex").await;
    let other_card = make_card(&repo, track.id.as_str(), "terminal").await;
    let root_session_id = start_root_runtime(&repo, &root_card).await;

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(track.id.as_str())
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
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        root, None,
        "deleting the root card must detach the track root"
    );
}

#[tokio::test]
async fn card_delete_sweeps_card_overlays() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
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
async fn track_delete_sweeps_card_overlays() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let card1 = make_card(&repo, w.id.as_str(), "terminal").await;
    let card2 = make_card(&repo, w.id.as_str(), "terminal").await;

    make_overlay(&repo, "p", "card", card1.id.as_str(), "status").await;
    make_overlay(&repo, "p", "card", card2.id.as_str(), "status").await;

    repo.track_delete(w.id.as_str()).await.unwrap();

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
async fn track_delete_sweeps_track_and_view_overlays() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    make_overlay(&repo, "p", "track", w.id.as_str(), "status").await;
    make_overlay(&repo, "p", "view", w.id.as_str(), "status").await;

    repo.track_delete(w.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("track", w.id.as_str())
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
async fn area_delete_sweeps_all_overlays_transitively() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    make_overlay(&repo, "p", "area", c.id.as_str(), "status").await;

    let tracks = [
        make_track(&repo, c.id.as_str(), "w1").await,
        make_track(&repo, c.id.as_str(), "w2").await,
    ];
    let mut card_ids: Vec<String> = Vec::new();

    for track in &tracks {
        make_overlay(&repo, "p", "track", track.id.as_str(), "status").await;
        make_overlay(&repo, "p", "view", track.id.as_str(), "status").await;

        for name in ["c1", "c2"] {
            let card = make_card(&repo, track.id.as_str(), name).await;
            make_overlay(&repo, "p", "card", card.id.as_str(), "status").await;
            card_ids.push(card.id.to_string());
        }
    }

    repo.area_delete(c.id.as_str()).await.unwrap();

    assert!(
        repo.overlays_for("area", c.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    for track in &tracks {
        assert!(
            repo.overlays_for("track", track.id.as_str())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.overlays_for("view", track.id.as_str())
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
// `terminals.card_id`: deleting a card / track / area silently nuked the
// terminal row beneath it. Issue #197 inverted that contract: the FK is now
// `ON DELETE RESTRICT` (migration 0011) so the schema **refuses** to nuke
// the terminal row implicitly — eager teardown in the route handlers
// (`routes/cards.rs::delete_card`, `routes/tracks.rs::delete_track`,
// `routes/areas.rs::delete_area`) owns the kill-daemon-unlink-socket
// sequence and explicitly drops the terminal row before the parent.
//
// The tests below now verify the RESTRICT semantics at the bare
// `Repo::card_delete` / `track_delete` / `area_delete` surface: a card/
// track/area that has a live terminal underneath cannot be deleted; once
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
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
async fn fk_restrict_track_delete_blocked_by_terminal_under_card() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    // Unrelated track/card/terminal that must NOT be touched on either
    // attempt (the second attempt succeeds, but only on `w`'s subtree).
    let other_track = make_track(&repo, c.id.as_str(), "other").await;
    let other_card = make_card(&repo, other_track.id.as_str(), "terminal").await;
    let other_term = make_terminal(&repo, other_card.id.as_str()).await;

    // RESTRICT bites: the track-delete cascade through `cards.track_id`
    // would try to delete `card`, which still has `term` pointing at
    // it — schema refuses.
    let err = repo.track_delete(w.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "expected an FK constraint error from sqlx, got: {err:?}"
    );
    assert!(repo.track_get(w.id.as_str()).await.unwrap().is_some());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_some());
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_some());

    // Drain the terminal first (the eager-teardown shape), then the
    // track delete clears the rest via CASCADE on `cards.track_id`.
    repo.terminal_delete(term.id.as_str()).await.unwrap();
    repo.track_delete(w.id.as_str()).await.unwrap();
    assert!(repo.track_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());

    // Sibling subtree intact across both attempts.
    assert!(
        repo.track_get(other_track.id.as_str())
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
async fn fk_restrict_area_delete_blocked_by_terminal_under_subtree() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let card = make_card(&repo, w.id.as_str(), "terminal").await;
    let term = make_terminal(&repo, card.id.as_str()).await;

    let err = repo.area_delete(c.id.as_str()).await.unwrap_err();
    assert!(
        matches!(err, CalmError::Db(_)),
        "expected an FK constraint error from sqlx, got: {err:?}"
    );
    assert!(repo.area_get(c.id.as_str()).await.unwrap().is_some());
    assert!(repo.track_get(w.id.as_str()).await.unwrap().is_some());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_some());
    assert!(repo.terminal_get(term.id.as_str()).await.unwrap().is_some());

    repo.terminal_delete(term.id.as_str()).await.unwrap();
    repo.area_delete(c.id.as_str()).await.unwrap();
    assert!(repo.area_get(c.id.as_str()).await.unwrap().is_none());
    assert!(repo.track_get(w.id.as_str()).await.unwrap().is_none());
    assert!(repo.card_get(card.id.as_str()).await.unwrap().is_none());
}

// ----------------------------------------------------- Sort defaulting ----

#[tokio::test]
async fn sort_defaulting_assigns_1_2_3_for_areas() {
    let repo = fresh_repo().await;
    let a = make_area(&repo, "a").await;
    let b = make_area(&repo, "b").await;
    let c = make_area(&repo, "c").await;
    assert_eq!(a.sort, 1.0);
    assert_eq!(b.sort, 2.0);
    assert_eq!(c.sort, 3.0);
}

#[tokio::test]
async fn sort_defaulting_is_scoped_per_area_for_tracks() {
    let repo = fresh_repo().await;
    let c1 = make_area(&repo, "c1").await;
    let c2 = make_area(&repo, "c2").await;
    let w1a = make_track(&repo, c1.id.as_str(), "w1a").await;
    let w1b = make_track(&repo, c1.id.as_str(), "w1b").await;
    let w2a = make_track(&repo, c2.id.as_str(), "w2a").await;
    assert_eq!(w1a.sort, 1.0);
    assert_eq!(w1b.sort, 2.0);
    // w2a is the first track in c2 so it should also start at 1.0.
    assert_eq!(w2a.sort, 1.0);
}

#[tokio::test]
async fn sort_defaulting_is_scoped_per_track_for_cards() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "c").await;
    let w1 = make_track(&repo, c.id.as_str(), "w1").await;
    let w2 = make_track(&repo, c.id.as_str(), "w2").await;
    let c1a = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c1b = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c1c = make_card(&repo, w1.id.as_str(), "terminal").await;
    let c2a = make_card(&repo, w2.id.as_str(), "terminal").await;
    assert_eq!(c1a.sort, 1.0);
    assert_eq!(c1b.sort, 2.0);
    assert_eq!(c1c.sort, 3.0);
    assert_eq!(c2a.sort, 1.0);
}

// ------------------------------------------------------- track_detail ----

#[tokio::test]
async fn track_detail_includes_sorted_cards_and_scoped_overlays() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
    let other_w = make_track(&repo, c.id.as_str(), "other").await;

    // Create cards in an out-of-order manner; expect sort = 1,2,3 sequential.
    let card_a = make_card(&repo, w.id.as_str(), "a").await;
    let card_b = make_card(&repo, w.id.as_str(), "b").await;
    let card_c = make_card(&repo, w.id.as_str(), "c").await;
    let other_card = make_card(&repo, other_w.id.as_str(), "other").await;

    // Overlays: one track-scoped, one card-scoped (on card_b), and one on a
    // card in an unrelated track (must be excluded).
    let track_overlay = repo
        .overlay_upsert(NewOverlay {
            plugin_id: "p".into(),
            entity_kind: "track".into(),
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
        .track_detail(w.id.as_str())
        .await
        .unwrap()
        .expect("track detail");
    assert_eq!(detail.track.id, w.id);
    let card_ids: Vec<&str> = detail.cards.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        card_ids,
        vec![card_a.id.as_str(), card_b.id.as_str(), card_c.id.as_str()]
    );

    let overlay_ids: std::collections::HashSet<&str> =
        detail.overlays.iter().map(|o| o.id.as_str()).collect();
    assert!(overlay_ids.contains(track_overlay.id.as_str()));
    assert!(overlay_ids.contains(card_overlay.id.as_str()));
    assert_eq!(detail.overlays.len(), 2);
}

#[tokio::test]
async fn track_detail_returns_none_for_missing_track() {
    let repo = fresh_repo().await;
    assert!(repo.track_detail("nonexistent").await.unwrap().is_none());
}

// --------------------------------------------------------- overlays ----

#[tokio::test]
async fn overlay_upsert_is_idempotent_on_unique_key() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    let p = NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "track".into(),
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

    let all = repo.overlays_for("track", w.id.as_str()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].payload, json!({"v": 2}));

    repo.overlay_delete("p", "track", w.id.as_str(), "status")
        .await
        .unwrap();
    let err = repo
        .overlay_delete("p", "track", w.id.as_str(), "status")
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::NotFound(_)));
}

#[tokio::test]
async fn overlays_by_kind_returns_all_track_overlays_across_areas() {
    let repo = fresh_repo().await;
    let c1 = make_area(&repo, "C1").await;
    let c2 = make_area(&repo, "C2").await;
    let w1 = make_track(&repo, c1.id.as_str(), "W1").await;
    let w2 = make_track(&repo, c2.id.as_str(), "W2").await;
    let card = make_card(&repo, w1.id.as_str(), "terminal").await;

    // Two track overlays in different areas + one card overlay.
    repo.overlay_upsert(NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "track".into(),
        entity_id: w1.id.to_string(),
        kind: "status".into(),
        payload: json!({"state": "running"}),
    })
    .await
    .unwrap();
    repo.overlay_upsert(NewOverlay {
        plugin_id: "p".into(),
        entity_kind: "track".into(),
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

    let tracks = repo.overlays_by_kind("track").await.unwrap();
    assert_eq!(tracks.len(), 2);
    let ids: std::collections::HashSet<&str> =
        tracks.iter().map(|o| o.entity_id.as_str()).collect();
    assert!(ids.contains(w1.id.as_str()));
    assert!(ids.contains(w2.id.as_str()));
    assert!(tracks.iter().all(|o| o.entity_kind == "track"));

    let cards = repo.overlays_by_kind("card").await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].entity_id, card.id.as_str());
}

// --------------------------------------------------------- terminals ----

#[tokio::test]
async fn terminal_create_rejects_duplicate_card_id() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        None,
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
async fn card_with_terminal_create_tx_rolls_back_on_invalid_track() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    // Sanity: track has no cards yet, and no orphan terminals exist.
    assert!(repo.cards_by_track(w.id.as_str()).await.unwrap().is_empty());

    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        &calm_server::model::new_id(),
        None,
        "track-that-does-not-exist".into(),
        None,
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
    .expect_err("unknown track must error");
    // Explicit rollback so the txn doesn't linger; would be implicit on drop
    // but we make the intent visible.
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::NotFound(_)));

    // No card was left behind in the valid track (it never had any), and no
    // terminal row exists at all — direct sqlx count against the table.
    let cards_in_w = repo.cards_by_track(w.id.as_str()).await.unwrap();
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let (card, _term) = calm_server::db::sqlite::card_with_terminal_create_tx(
        &mut tx,
        calm_server::model::new_id(),
        &calm_server::model::new_id(),
        None,
        w.id.clone(),
        None,
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    // Pre-seed two cards so the next sort default lands at 3.0 — same
    // assertion shape as `sort_defaulting_is_scoped_per_track_for_cards`.
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

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
async fn card_with_codex_create_tx_rolls_back_on_invalid_track() {
    let repo = fresh_repo().await;
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

    assert!(repo.cards_by_track(w.id.as_str()).await.unwrap().is_empty());

    let card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let err = calm_server::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card_id,
        &calm_server::model::new_id(),
        None,
        "track-that-does-not-exist".into(),
        None,
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
    .expect_err("unknown track must error");
    tx.rollback().await.unwrap();

    assert!(matches!(err, CalmError::NotFound(_)));

    let cards_in_w = repo.cards_by_track(w.id.as_str()).await.unwrap();
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;

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
        None,
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

/// #1284 S1 review round 3 (P2-3). `PATCH /api/plugins/{id}/config` documents
/// itself as the only writer of an installed plugin's `user_config`, and the
/// 409-plus-`?reset=true` design for a corrupt row rests entirely on that
/// sentence. It used to be true only because `PluginHost::install` refuses a
/// duplicate id before reaching the upsert — a statement propped up by a check
/// in another crate, which is not where it can be relied on.
///
/// So the SQL carries it: `plugin_install`'s `ON CONFLICT DO UPDATE` set
/// leaves `user_config` alone. Everything else in that set still updates,
/// which is the half that must not regress.
#[tokio::test]
async fn plugin_install_upsert_never_resets_operator_config() {
    let repo = fresh_repo().await;
    repo.plugin_install(sample_new_plugin("p.cfg", false))
        .await
        .unwrap();
    repo.plugin_update_user_config("p.cfg", json!({ "theme": "light" }))
        .await
        .unwrap();

    // A second install of the same id — what the upsert branch is for — passes
    // the `{}` every fresh install passes.
    let mut np = sample_new_plugin("p.cfg", true);
    np.version = "0.2.0".into();
    let after = repo.plugin_install(np).await.unwrap();

    assert_eq!(
        after.user_config,
        json!({ "theme": "light" }),
        "the upsert must not be a second writer of user_config"
    );
    assert_eq!(
        after.version, "0.2.0",
        "…while the rest of the set still updates"
    );
    assert!(after.enabled);
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
    let c = make_area(&repo, "C").await;
    let w = make_track(&repo, c.id.as_str(), "W").await;
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
async fn shared_initial_prompt_takeover_returns_live_pending_shared_planners() {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::model::{CardRole, NewCard};

    let repo = fresh_repo().await;
    let c = make_area(&repo, "shared-boot-exclusion").await;
    let mapped_track = make_track(&repo, c.id.as_str(), "mapped").await;
    let pending_track = make_track(&repo, c.id.as_str(), "").await;
    let phantom_track = make_track(&repo, c.id.as_str(), "phantom").await;
    let cache = CardRoleCache::new();

    let pending_card_id = calm_server::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    let mapped = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            track_id: mapped_track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "codex_source": "shared",
                "codex_thread_id": "T-shared-mapped",
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Planner,
        false,
        &cache,
    )
    .await
    .expect("create mapped shared planner card");
    let pending = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        pending_card_id.clone(),
        NewCard {
            track_id: pending_track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Planner,
        false,
        &cache,
    )
    .await
    .expect("create pending shared planner card");
    let phantom = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            track_id: phantom_track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Planner,
        false,
        &cache,
    )
    .await
    .expect("create deferred placeholder shared planner card");
    // INV-CHAT-015 has two independent production fences: c.role = 'planner'
    // and ws.contract = 'planner'. This counterexample pins the role fence;
    // the contract fence is pinned separately by INV-CHAT-009's counterexample.
    let chat = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            track_id: pending_track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "harness_profile": "plain_chat",
                "appserver_sock": "unix:///tmp/shared.sock",
            }),
        },
        CardRole::Worker,
        false,
        &cache,
    )
    .await
    .expect("create plain-chat card");
    tx.commit().await.unwrap();

    // Shared takeover now keys off an active shared-spec runtime pointing
    // at a live terminal, not payload identity stamps.
    let mapped_term = make_terminal(&repo, mapped.id.as_str()).await;
    let term = make_terminal(&repo, pending.id.as_str()).await;
    let chat_term = make_terminal(&repo, chat.id.as_str()).await;
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: calm_server::model::new_id(),
            card_id: mapped.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
            card_id: chat.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some(chat_term.id.to_string()),
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
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: calm_server::model::new_id(),
            card_id: pending.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
    session_prepare_deferred_planner_tx(
        &mut tx,
        &WorkerSessionInit {
            id: phantom_session_id.clone(),
            card_id: phantom.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
        repo.shared_planner_cards_for_initial_prompt_takeover()
            .await
            .expect("shared pending takeover query"),
        vec![(
            pending.id.to_string(),
            pending_track.id.to_string(),
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
        repo.shared_planner_cards_for_initial_prompt_takeover()
            .await
            .expect("shared pending takeover query after terminal exit")
            .is_empty(),
        "exited terminal must drop the card from shared pending takeover"
    );
}
