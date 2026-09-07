//! Issue #1444 — the shared Codex daemon's in-memory `thread_id -> card_id`
//! attribution must converge with a Track/Area delete **commit**, driven
//! through the real `DELETE /api/tracks/{id}` and `DELETE /api/areas/{id}`
//! routes.
//!
//! What is actually at stake: `SharedCodexAppServer::resume_cached_threads`
//! resumes every entry of `thread_cache` on a daemon (re)connect. The cold
//! `start_or_takeover` path rebuilds that map from the database first, so it
//! self-heals; the crash/respawn path (`transition_replace`) does **not**, so a
//! stale entry there is resumed for a Card that no longer has a database owner.
//! Every assertion below therefore reads
//! `SharedCodexAppServer::resume_candidates_for_test` — the same accessor the
//! production resume loop iterates — or `cached_card_for_thread`, and never a
//! re-statement of the rule inside the fixture.
//!
//! #1393 already owns the active-turn quiesce, Harness shutdown, lifecycle
//! fence and workspace compensation. Nothing here re-tests those.

#![cfg(unix)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::{
    SharedCodexAppServer, SharedThreadStartParams, ThreadConfig,
};
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness — same shape as `track_workspace_recycle`'s, plus the pending
// registry, because `handle_thread_started_notification` returns early without
// one and the late-`thread/started` race below needs that path to really run.
// ---------------------------------------------------------------------------

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    shared_codex: Arc<SharedCodexAppServer>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    sqlx_repo.seed_card_role_cache(&roles).await.unwrap();
    sqlx_repo.seed_track_area_cache(&tracks).await.unwrap();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let pending = Arc::new(PendingThreadStartRegistry::new(
        sqlx_repo.clone(),
        events.clone(),
    ));
    let shared_codex =
        SharedCodexAppServer::new_fake_running_with_pending(sqlx_repo.clone(), Some(pending));
    let state = AppState::from_parts(
        repo,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles.clone()),
        Some(tracks.clone()),
    )
    .with_shared_codex_appserver(shared_codex.clone())
    .with_workspace_root(workspace_root);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo: sqlx_repo,
        shared_codex,
        _tmp: tmp,
    }
}

fn theme() -> Value {
    json!({"fg": [255, 255, 255], "bg": [0, 0, 0]})
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn create_area(b: &Boot, name: &str) -> String {
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/areas",
        Some(json!({"name": name, "color": "#abc"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let area: Value = serde_json::from_str(&body).unwrap();
    area["id"].as_str().unwrap().to_string()
}

async fn managed_track(b: &Boot, area_id: &str, title: &str) -> String {
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({"area_id": area_id, "title": title, "theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let track: Value = serde_json::from_str(&text).unwrap();
    track["id"].as_str().unwrap().to_string()
}

async fn card_ids(b: &Boot, track_id: &str) -> Vec<String> {
    let cards = b.repo.cards_by_track(track_id).await.unwrap();
    assert!(
        !cards.is_empty(),
        "premise: a created track owns at least one card"
    );
    cards.iter().map(|card| card.id.to_string()).collect()
}

/// Put a `thread_id -> card_id` mapping into the shared daemon through the
/// production kernel mint (`thread_start_mint_for_card`), which is how planner
/// and worker threads really enter `thread_cache`.
async fn mint_thread(b: &Boot, card_id: &str) -> String {
    b.shared_codex
        .thread_start_mint_for_card(
            card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap()
}

/// Mint a mapping for every card of `track_id` and return the pairs.
async fn mint_track_threads(b: &Boot, track_id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for card_id in card_ids(b, track_id).await {
        let thread_id = mint_thread(b, &card_id).await;
        out.push((thread_id, card_id));
    }
    out
}

/// The pairs a daemon reconnect would resume, read through the production
/// accessor `resume_cached_threads` itself iterates.
fn resume_candidates(b: &Boot) -> Vec<(String, String)> {
    b.shared_codex.resume_candidates_for_test()
}

fn assert_resumable(b: &Boot, pairs: &[(String, String)]) {
    let candidates = resume_candidates(b);
    for pair in pairs {
        assert!(
            candidates.contains(pair),
            "expected {pair:?} to still be resumable; candidates={candidates:?}"
        );
        assert_eq!(
            b.shared_codex.cached_card_for_thread(&pair.0).as_deref(),
            Some(pair.1.as_str())
        );
    }
}

fn assert_not_resumable(b: &Boot, pairs: &[(String, String)]) {
    let candidates = resume_candidates(b);
    for pair in pairs {
        assert!(
            !candidates.contains(pair),
            "a deleted card's thread is still resumable: {pair:?}; candidates={candidates:?}"
        );
        assert_eq!(
            b.shared_codex.cached_card_for_thread(&pair.0),
            None,
            "thread {} still resolves to a deleted card",
            pair.0
        );
    }
}

// ---------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------

/// Deleting a Track drops the mapping of every Card it owned — and only those.
#[tokio::test]
async fn deleting_a_track_drops_its_cards_thread_mappings_and_spares_its_sibling() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let victim = managed_track(&b, &area_id, "victim").await;
    let survivor = managed_track(&b, &area_id, "survivor").await;
    let victim_pairs = mint_track_threads(&b, &victim).await;
    let survivor_pairs = mint_track_threads(&b, &survivor).await;
    assert_resumable(&b, &victim_pairs);

    let (status, body) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{victim}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.track_get(&victim).await.unwrap().is_none());

    assert_not_resumable(&b, &victim_pairs);
    assert_resumable(&b, &survivor_pairs);
}

/// Deleting an Area drops every member Track's Card mappings; another Area's
/// mappings are untouched.
#[tokio::test]
async fn deleting_an_area_drops_member_mappings_and_keeps_other_areas() {
    let b = boot().await;
    let victim_area = create_area(&b, "Atlas").await;
    let other_area = create_area(&b, "Beta").await;
    let first = managed_track(&b, &victim_area, "first").await;
    let second = managed_track(&b, &victim_area, "second").await;
    let bystander = managed_track(&b, &other_area, "bystander").await;
    let mut victim_pairs = mint_track_threads(&b, &first).await;
    victim_pairs.extend(mint_track_threads(&b, &second).await);
    let bystander_pairs = mint_track_threads(&b, &bystander).await;
    assert_resumable(&b, &victim_pairs);

    let (status, body) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/areas/{victim_area}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.area_get(&victim_area).await.unwrap().is_none());

    assert_not_resumable(&b, &victim_pairs);
    assert_resumable(&b, &bystander_pairs);
}

/// A Track delete that does not commit must leave its mappings usable. The
/// panic arrives after the workspace recycle and before the transaction, i.e.
/// on the compensation path, which must not clean anything.
#[tokio::test]
async fn a_rolled_back_track_delete_keeps_its_thread_mappings() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let track_id = managed_track(&b, &area_id, "survives its own delete").await;
    let pairs = mint_track_threads(&b, &track_id).await;

    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: true,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{track_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        b.repo.track_get(&track_id).await.unwrap().is_some(),
        "premise: the delete rolled back"
    );
    assert_resumable(&b, &pairs);
}

/// The Area twin, on the arm that actually models a failed database commit:
/// `finish_area_deletion` never runs, the workspace compensation does, and the
/// mappings must survive both.
#[tokio::test]
async fn a_failed_area_delete_commit_keeps_its_thread_mappings() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let track_id = managed_track(&b, &area_id, "area rolls back").await;
    let pairs = mint_track_threads(&b, &track_id).await;

    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: true,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/areas/{area_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        b.repo.area_get(&area_id).await.unwrap().is_some(),
        "premise: the area delete rolled back"
    );
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert_resumable(&b, &pairs);
}

/// The concurrent regression through the real deletion entry point: a
/// `thread/started` for the victim Card is handled while `DELETE
/// /api/tracks/{id}` is parked between the workspace recycle and its
/// transaction. The notification re-inserts the mapping — asserted, so the
/// interleaving is real and not merely hoped for — and the commit must still
/// leave nothing behind.
#[tokio::test]
async fn a_late_thread_started_during_a_track_delete_leaves_no_mapping() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let track_id = managed_track(&b, &area_id, "late notification").await;
    // The database attribution a late `thread/started` resolves through: the
    // track's live planner worker session, one of the rows the delete
    // transaction removes.
    let late_thread = "T-late-1444";
    let (session_id, card_id): (String, String) = sqlx::query_as(
        "SELECT id, card_id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let updated =
        sqlx::query("UPDATE worker_sessions SET provider='codex', thread_id=?1 WHERE id=?2")
            .bind(late_thread)
            .bind(&session_id)
            .execute(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(updated.rows_affected(), 1);

    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: false,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track_id, hook.clone());

    let delete_app = b.app.clone();
    let delete_id = track_id.clone();
    let delete_task = tokio::spawn(async move {
        request(
            delete_app,
            "DELETE",
            &format!("/api/tracks/{delete_id}"),
            None,
        )
        .await
    });
    hook.entered.notified().await;

    // Inside the window: the row is still there, so the notification really
    // does re-establish the mapping the delete is about to invalidate.
    b.shared_codex
        .handle_thread_started_notification_for_test(late_thread)
        .await
        .unwrap();
    assert_eq!(
        b.shared_codex
            .cached_card_for_thread(late_thread)
            .as_deref(),
        Some(card_id.as_str()),
        "premise: the late thread/started re-inserted the mapping mid-delete"
    );

    hook.release.notify_one();
    let (status, body) = delete_task.await.unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.track_get(&track_id).await.unwrap().is_none());
    assert_not_resumable(&b, &[(late_thread.to_string(), card_id)]);
}

/// The two operations are on ONE serialization boundary, not merely agreeing
/// on an end state. Holding the production `kernel_thread_start_serial` must
/// park both; if the cleanup took a different lock (or none), it would finish
/// while the guard is held and this test would go green for the wrong reason —
/// so the parked-ness is asserted before the guard is released.
#[tokio::test]
async fn cleanup_and_thread_started_share_the_kernel_thread_start_serial() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let track_id = managed_track(&b, &area_id, "one boundary").await;
    let card_id = card_ids(&b, &track_id).await.remove(0);
    let thread_id = mint_thread(&b, &card_id).await;

    let guard = b.shared_codex.lock_thread_start_serial_for_test().await;

    let cleanup_daemon = b.shared_codex.clone();
    let cleanup_card = card_id.clone();
    let cleanup = tokio::spawn(async move {
        cleanup_daemon
            .forget_threads_for_deleted_cards(&HashSet::from([cleanup_card]))
            .await
    });
    let notify_daemon = b.shared_codex.clone();
    let notify = tokio::spawn(async move {
        notify_daemon
            .handle_thread_started_notification_for_test("T-blocked-1444")
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !cleanup.is_finished(),
        "the cleanup did not take kernel_thread_start_serial"
    );
    assert!(
        !notify.is_finished(),
        "premise: thread/started handling takes kernel_thread_start_serial"
    );
    assert_eq!(
        b.shared_codex.cached_card_for_thread(&thread_id).as_deref(),
        Some(card_id.as_str()),
        "nothing may be removed while the boundary is held"
    );

    drop(guard);
    let dropped = tokio::time::timeout(Duration::from_secs(5), cleanup)
        .await
        .expect("cleanup must proceed once the boundary is released")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), notify)
        .await
        .expect("thread/started handling must proceed once released")
        .unwrap()
        .unwrap();
    assert_eq!(dropped, 1);
    assert_eq!(b.shared_codex.cached_card_for_thread(&thread_id), None);
}

/// The consequence the issue is actually about: after the delete commits, a
/// daemon reconnect — hot takeover or cold respawn, both iterate this same
/// list — has nothing to resume for the deleted Card.
#[tokio::test]
async fn a_daemon_reconnect_would_not_resume_a_deleted_cards_thread() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let victim = managed_track(&b, &area_id, "gone").await;
    let survivor = managed_track(&b, &area_id, "stays").await;
    let victim_pairs = mint_track_threads(&b, &victim).await;
    let survivor_pairs = mint_track_threads(&b, &survivor).await;

    let (status, body) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{victim}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    // Creating a track also mints a planner thread, so the candidate list is
    // not exactly the threads this test minted. The invariant is stated over
    // ownership instead: not one candidate belongs to a deleted Card, and the
    // survivor's own threads are all still there.
    let victim_cards: HashSet<String> = victim_pairs
        .iter()
        .map(|(_, card_id)| card_id.clone())
        .collect();
    let candidates = resume_candidates(&b);
    assert!(!candidates.is_empty(), "premise: something is resumable");
    for (thread_id, card_id) in &candidates {
        assert!(
            !victim_cards.contains(card_id),
            "a reconnect would resume {thread_id} for deleted card {card_id}"
        );
    }
    assert_resumable(&b, &survivor_pairs);
    assert_not_resumable(&b, &victim_pairs);
}
