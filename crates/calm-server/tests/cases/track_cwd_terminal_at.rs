//! Issue #250 PR 2 — coverage for `Track.cwd`, `Track.terminal_at`, the
//! `POST /api/tracks` cwd-claim handling (attach_folder + resolve),
//! lifecycle terminal-stamp wiring inside `track_update_tx`, and the
//! calendar window query `GET /api/tracks?since&until&area_id`.
//!
//! These tests boot a stub-daemon router (no real codex / no real
//! terminal renderer) so the planner-push app-server boot fails on
//! `POST /api/tracks`. Issue #293 / PR #311 made that boot NON-FATAL —
//! the route now returns 201 (inert track) on that branch rather than
//! 500 — and the track + cards + (optional) area_folder rows land at
//! commit time regardless. The assertions below tolerate either 201 or
//! 500 (legacy) since they target DB state, the lifecycle → terminal_at
//! wiring, and the route-layer body shapes — none of them need the
//! daemon to actually exec the codex binary.
//!
//! Tests in `track_create_sync_daemon.rs` cover the real-daemon path
//! end-to-end (planner daemon cwd == track.cwd, codex argv carries title);
//! this file owns the wider behavioral surface that doesn't need a
//! real spawn.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{AreaKind, NewArea, TrackLifecycle, TrackPatch, TrackWorkspaceKind};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

/// #1147 S3 — `POST /api/tracks` now validates an explicit (attached) `cwd`
/// *before* the area-claim scan: absolute, existing, inside a Git work tree.
/// The claim-semantics fixtures below used invented literals (`/workspace`,
/// `/a/b`, `/srv/projects/alpha`) that never existed on any disk, so they now
/// name real, shared, idempotent Git work trees instead. Every ancestor /
/// descendant / disjoint relation the assertions depend on is reproduced by
/// construction (`attached_sub` is a real directory *inside* the named
/// fixture repository), and every assertion compares against the bound local
/// rather than a literal.
fn attached_sub(name: &str, sub: &str) -> String {
    let path = std::path::PathBuf::from(attached_repo_fixture(name)).join(sub);
    std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("create {path:?}: {e}"));
    path.to_string_lossy().into_owned()
}

struct Boot {
    app: axum::Router,
    area_id: String,
    /// #1147 S2 — the managed workspace root this boot was pinned to.
    workspace_root: std::path::PathBuf,
    /// A second area pre-created so cross-area conflict tests have a
    /// stable target. Used by the descendant/ancestor cases below.
    other_area_id: String,
    repo: Arc<dyn Repo>,
    /// Concrete `SqlxRepo` handle so the window-query test can write
    /// raw timestamps via `pool()`. The same backing pool as `repo`
    /// (both `Arc`s point at the same `SqlxRepo`).
    sqlx_repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "track-cwd-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let other = repo
        .area_create(NewArea {
            name: "other-area".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();

    // Stub daemon bin — planner card daemon spawn will fail at the
    // post-commit phase. The behaviors under test (track + folder row
    // shape, terminal_at stamps) all execute *before* the spawn, so
    // a 500 on the response is expected and the test asserts on DB
    // state instead.
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-cwd-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    )
    // #1147 S2 — omitted-cwd creates now allocate a managed workspace and
    // `git init` it. Pin the root inside this test's TempDir.
    .with_workspace_root(tmp.path().join("workspaces"));

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        area_id: area.id.to_string(),
        workspace_root: tmp.path().join("workspaces"),
        other_area_id: other.id.to_string(),
        repo,
        sqlx_repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Mirror `routes::codex_cards::default_cwd` + the route's `normalize_path`
/// (trim one trailing slash except `/`).
fn expected_default_cwd() -> String {
    let raw = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    if raw == "/" {
        "/".to_string()
    } else {
        raw.strip_suffix('/').unwrap_or(&raw).to_string()
    }
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ---------------------------------------------------------------------------
// POST /api/tracks — cwd validation + attach_folder path
// ---------------------------------------------------------------------------

/// Happy path 1: the body's area already claims an ancestor of cwd.
/// `attach_folder = false` is enough — no new folder row is needed.
/// Planner-daemon spawn will fail (stub bin); tolerate 201 or 500 but
/// assert the track row landed with the cwd verbatim.
#[tokio::test]
async fn post_api_tracks_uses_existing_folder_claim() {
    let boot = boot().await;

    // Pre-seed: the area claims the workspace root as a folder, and the
    // track's cwd is a real directory *under* it.
    let claimed = attached_repo_fixture("cwd-terminal-existing-claim");
    let cwd = attached_sub("cwd-terminal-existing-claim", "sub/dir");
    boot.repo
        .area_folder_create(&boot.area_id, &claimed)
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-existing-claim",
            "cwd": cwd.clone(),
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // Stub daemon: spawn may 500 post-commit; track row still lands.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1, "exactly one track created");
    assert_eq!(tracks[0].workspace.path, cwd);
    assert_eq!(tracks[0].terminal_at, None);
    assert_eq!(tracks[0].lifecycle, TrackLifecycle::Draft);

    // No extra folder row was minted (attach_folder = false +
    // existing claim covers cwd).
    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, claimed);
}

/// Happy path 2: cwd is unclaimed, body sets `attach_folder = true`.
/// The folder row + the track row land in the same tx.
#[tokio::test]
async fn post_api_tracks_with_attach_folder_creates_folder_and_track() {
    let boot = boot().await;

    let cwd = attached_repo_fixture("cwd-terminal-attach-alpha");
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-attach",
            "cwd": cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert!(status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR);

    // Folder claim landed.
    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, cwd);

    // Track row carries the same path.
    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].workspace.path, cwd);
}

/// Issue #275 — the area already claims *exactly* this cwd and the caller
/// still sets `attach_folder = true`. The claim scan finds the same area as
/// the owner, so `attach_folder` is silently ignored and no second row is
/// minted.
///
/// BEHAVIOR CHANGE (deliberate). Before this fix the in-tx insert ran
/// unconditionally on the scan result: it re-inserted `/workspace`, hit
/// `UNIQUE(area_folders.path)`, and the whole request 409'd. A caller
/// re-posting the folder it already owns is not a conflict, so 201 is the
/// correct answer. This test pins the new outcome.
#[tokio::test]
async fn post_api_tracks_attach_folder_is_idempotent_for_exact_same_area_claim() {
    let boot = boot().await;

    // The pre-seeded state below already satisfies `folders.len() == 1`,
    // so this test would also pass if folder enforcement were skipped
    // wholesale for this area via the `is_system_area` bypass in
    // `routes::tracks::create_track`. Pin that the area under test is NOT
    // a system area, so the assertions can only be explained by the
    // enforcement path actually running.
    let area = boot.repo.area_get(&boot.area_id).await.unwrap().unwrap();
    assert_eq!(
        area.kind,
        AreaKind::User,
        "this test only proves idempotency if folder enforcement is not bypassed"
    );

    let cwd = attached_repo_fixture("cwd-terminal-reclaim-exact");
    boot.repo
        .area_folder_create(&boot.area_id, &cwd)
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-reclaim-exact",
            "cwd": cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // The invariant this test defends: re-claiming your own folder is
    // NOT a conflict. (Stub daemon may still 500 post-commit.)
    assert_ne!(
        status,
        StatusCode::CONFLICT,
        "re-claiming the area's own folder must not 409; body={body}"
    );
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    // Exactly one claim row — no duplicate `/workspace`.
    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(
        folders.len(),
        1,
        "attach_folder on an already-owned exact path must not mint a second row; got {:?}",
        folders.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert_eq!(folders[0].path, cwd);

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1, "the track must have landed");
    assert_eq!(tracks[0].workspace.path, cwd);
}

/// Issue #275 — the area claims `/a` and the caller posts `cwd: "/a/b"`
/// with `attach_folder = true`. The scan finds the same area already
/// covering the cwd, so nothing is minted.
///
/// BEHAVIOR CHANGE (deliberate), and the important one: before this fix
/// the in-tx insert ran unconditionally on the scan result, so this
/// request created `/a/b` alongside the existing `/a` — two rows that both
/// cover `/a/b/...`. That is precisely the overlapping-claim corruption
/// `area_folders.rs::resolve_and_track_create_agree_on_overlapping_rows`
/// has to seed through the raw repo primitive to reproduce; this arm
/// handed it to any caller over plain HTTP, single-threaded, with no
/// concurrency at all. It was the larger of the two holes in the overlap
/// invariant (the other being the scan/insert TOCTOU).
#[tokio::test]
async fn post_api_tracks_attach_folder_does_not_mint_overlapping_descendant() {
    let boot = boot().await;

    let claimed = attached_repo_fixture("cwd-terminal-overlap");
    let cwd = attached_sub("cwd-terminal-overlap", "b");
    boot.repo
        .area_folder_create(&boot.area_id, &claimed)
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-reclaim-descendant",
            "cwd": cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::CONFLICT,
        "a cwd already covered by this area's own claim must not 409; body={body}"
    );
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    // The overlapping row must NOT exist. `/a` alone still covers `/a/b`.
    let folders = boot.repo.area_folders_list_all().await.unwrap();
    let paths: Vec<&str> = folders.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![claimed.as_str()],
        "attach_folder must not mint `{cwd}` under an area that already claims \
         `{claimed}` — two rows covering the same subtree is the corrupt state \
         #275 exists to prevent"
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1, "the track must have landed");
    assert_eq!(tracks[0].workspace.path, cwd);
}

/// `attach_folder = false` with an unclaimed cwd is refused (409) —
/// otherwise the track would be orphaned (no area resolves it).
#[tokio::test]
async fn post_api_tracks_rejects_unclaimed_cwd_without_attach_folder() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-orphan",
            "cwd": attached_repo_fixture("cwd-terminal-unclaimed"),
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");
    // No track / folder rows should have landed.
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

/// `attach_folder = true` against a cwd that already conflicts with
/// another area's claim is refused (409) with the structured
/// `FolderConflict` body, and the whole tx rolls back (no track row,
/// no extra folder row).
#[tokio::test]
async fn post_api_tracks_attach_folder_conflict_rolls_back() {
    let boot = boot().await;

    // Pre-seed the *other* area with a folder that overlaps the cwd
    // we're about to try claiming.
    let other_claim = attached_repo_fixture("cwd-terminal-shared");
    let cwd = attached_sub("cwd-terminal-shared", "inner");
    boot.repo
        .area_folder_create(&boot.other_area_id, &other_claim)
        .await
        .unwrap();
    let folders_before = boot.repo.area_folders_list_all().await.unwrap().len();
    assert_eq!(folders_before, 1);

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-conflict",
            "cwd": cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");

    // The structured 409 body carries the conflicting folder. Match
    // any of `equal | ancestor | descendant` since the route may
    // classify either side as the canonical kind; the issue's
    // requirement is just that the conflict is precisely surfaced.
    let kind = body
        .get("conflict_kind")
        .and_then(Value::as_str)
        .expect("structured FolderConflict body");
    assert!(
        matches!(kind, "descendant" | "ancestor" | "equal"),
        "unexpected conflict kind `{kind}` in body {body}",
    );

    // Rollback: no new track, no new folder.
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    let folders_after = boot.repo.area_folders_list_all().await.unwrap().len();
    assert_eq!(
        folders_after, folders_before,
        "attach_folder = true must roll back the folder insert on conflict; \
         folder count before = {folders_before}, after = {folders_after}"
    );
}

/// `attach_folder = false` against a cwd that resolves to *another*
/// area must 409 — the track's area and the folder's area must agree.
#[tokio::test]
async fn post_api_tracks_rejects_cwd_owned_by_another_area() {
    let boot = boot().await;

    let other_claim = attached_repo_fixture("cwd-terminal-owned-by-other");
    let cwd = attached_sub("cwd-terminal-owned-by-other", "sub");
    boot.repo
        .area_folder_create(&boot.other_area_id, &other_claim)
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-cross",
            "cwd": cwd.clone(),
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");
    let conflict_area = body
        .get("area_id")
        .and_then(Value::as_str)
        .expect("structured body");
    assert_eq!(conflict_area, boot.other_area_id);

    // No track on either area.
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .tracks_by_area(&boot.other_area_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

/// System area (kernel-internal scaffolding) is exempt from the
/// area_folders claim namespace: a track POST against it must not
/// mint a area_folders row even when `attach_folder = true`, and
/// must not poison the global descendant check for subsequent user
/// areas. Regression for the `cwd: '/'` self-collision noticed in CI.
///
/// #1147 S3 — the literal `/` cannot be the system cwd any more: an explicit
/// `cwd` is validated (absolute, exists, inside a Git work tree) *before* the
/// system-area exemption runs, and `/` is not inside a work tree. What made
/// `/` the sharp case was that it is an **ancestor of every other cwd**, so
/// the system area is given a real repository root here and the user track
/// below is given a real directory *inside* it. The poison the regression is
/// about is reproduced exactly: had the system area claimed its root, the
/// user create underneath it would 409.
#[tokio::test]
async fn post_api_tracks_for_system_area_skips_folder_claim() {
    let boot = boot().await;

    // Mint the system area via its idempotent route.
    let (status, body) = post(boot.app.clone(), "/api/areas/system", json!({})).await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::OK,
        "mint system area: status={status}, body={body}",
    );
    let system_area_id = body["id"].as_str().expect("system area id").to_string();
    let system_cwd = attached_repo_fixture("cwd-terminal-system-root");
    let user_cwd = attached_sub("cwd-terminal-system-root", "beta");

    // POST a track with the system area + a `/` cwd + attach_folder=true.
    // Pre-fix this would claim `/` for the system area and poison every
    // subsequent user track (descendant-of-`/` 409).
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": system_area_id,
            "title": "Today",
            "cwd": system_cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "system-area track create: status={status}"
    );

    // No area_folders row landed for the system area.
    let sys_folders = boot
        .repo
        .area_folders_by_area(&system_area_id)
        .await
        .unwrap();
    assert!(
        sys_folders.is_empty(),
        "system area must not appear in area_folders, got: {sys_folders:?}"
    );

    // And a subsequent user-area track with a normal cwd works — the
    // system area's `/` cwd is *not* a descendant-blocker.
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "user track",
            "cwd": user_cwd.clone(),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "user-area track create after system: status={status}"
    );
    let user_folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(user_folders.len(), 1);
    assert_eq!(user_folders[0].path, user_cwd);
}

/// Issue #1131 — omitted `cwd` (and `attach_folder`) is not the same as
/// sending `cwd: "$HOME"`. Omission skips `area_folders` entirely. An
/// *explicit* HOME path with `attach_folder: false` and no prior claim still
/// 409s — that is `post_api_tracks_rejects_unclaimed_cwd_without_attach_folder`.
/// Do not special-case an explicit HOME path; only omission takes this
/// branch. Never claim `$HOME` into `area_folders` (longest-prefix
/// would poison every other area).
///
/// #1147 S2 — what omission *stores* changed. It used to persist
/// `default_cwd()` (`$HOME`), which is not a git repository, so every
/// `kind: codex` task on such a track died in `git rev-parse --show-toplevel`
/// with nothing but `spawn-failed` to show for it — the defect #1147 opened
/// on. Omission is now the managed-default branch: the server allocates
/// `<workspace-root>/<area_id>/<track_id>` and materializes it. The
/// `area_folders`-untouched half of this test is unchanged and still the
/// point of the #1131 branch.
#[tokio::test]
async fn post_api_tracks_omitted_cwd_allocates_managed_and_skips_area_folders() {
    let boot = boot().await;
    assert_eq!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .len(),
        0,
        "fixture area must start with no claims so 'unchanged' is empty"
    );

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-title-only",
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // Stub daemon: spawn may 500 post-commit; track row still lands.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1, "exactly one track created");
    assert_eq!(tracks[0].title, "w-title-only");
    assert_eq!(tracks[0].workspace.kind, TrackWorkspaceKind::Managed);
    assert_eq!(
        std::path::PathBuf::from(&tracks[0].workspace.path),
        boot.workspace_root
            .join(&boot.area_id)
            .join(tracks[0].id.as_str())
    );
    assert_ne!(
        tracks[0].workspace.path,
        expected_default_cwd(),
        "the pre-#1147 behavior was `$HOME`, which is not a repository"
    );

    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert!(
        folders.is_empty(),
        "omitted cwd must not mint a area_folders row; got {folders:?}"
    );

    // `cwd: null` is the same omitted branch as a missing key.
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-null-cwd",
            "cwd": null,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "null cwd: expected 201 or 500; got {status} body={body}",
    );
    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 2, "null cwd must also mint a track");
    assert!(
        tracks
            .iter()
            .all(|w| w.workspace.kind == TrackWorkspaceKind::Managed
                && std::path::Path::new(&w.workspace.path)
                    .starts_with(boot.workspace_root.join(&boot.area_id))),
        "`cwd: null` must take the same managed-default branch as omission: {tracks:?}"
    );
    assert!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .is_empty(),
        "null cwd must not mint a area_folders row"
    );
}

/// Explicit `cwd: $HOME` is *not* the omitted-cwd branch. A user area
/// with no claims is still refused when `attach_folder` is false —
/// production only skips the scan when `cwd` is missing/`null`. Do not
/// special-case HOME as a present path; that would poison every other area via
/// longest-prefix if it ever claimed.
///
/// #1147 S3 — an explicit `cwd` is now validated (absolute, exists, inside a
/// Git work tree) *before* the claim scan, and `$HOME` is a path this test
/// does not own. On a machine whose HOME is not a work tree the request is
/// refused at that gate with a 400 — which is the #1147 defect made eager,
/// since the pre-S2 omitted branch stored exactly this path and every
/// `kind: codex` worker on such a track then died in `git rev-parse` with
/// nothing but `spawn-failed`.
///
/// The refusal pinned here is the 400 at the workspace gate. This used to be a
/// `if home_is_work_tree { 409 } else { 400 }` either/or, and the 409 arm never
/// ran: a home directory is not a Git work tree on the machines this suite
/// runs on. All the conditional did was hide which behaviour the test actually
/// holds. The probe survives as a *precondition assertion*: where HOME
/// really is a work tree, this fails loudly with an explanation instead of
/// silently exercising a different code path under the same test name.
///
/// The probe builds its git command the way the server does
/// (`neige_git_command`, which scrubs `GIT_DIR` / `GIT_WORK_TREE` /
/// `GIT_CEILING_DIRECTORIES` / `GIT_CONFIG_*`). A bare `git` here would be
/// answering a question the server never asks.
#[tokio::test]
async fn post_api_tracks_explicit_home_cwd_without_attach_folder_is_refused() {
    let boot = boot().await;
    let home = expected_default_cwd();
    assert!(
        home.starts_with('/'),
        "fixture HOME/default_cwd must be absolute so this hits the workspace \
         gate, not the not-absolute 400; got `{home}`"
    );
    let home_is_work_tree = calm_server::test_seams::neige_git_command_for_test()
        .arg("-C")
        .arg(&home)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    assert!(
        !home_is_work_tree,
        "precondition: this test pins the attached-workspace gate's 400, which \
         requires HOME (`{home}`) not to be a Git work tree. It is one here, so \
         the request would instead reach the claim scan and 409. Re-point HOME \
         for the suite, or split this into two tests — do not weaken the \
         assertion back into an either/or."
    );

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-explicit-home",
            "cwd": home,
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "explicit HOME must be refused at the attached-workspace gate, never \
         taken down the managed branch; body = {body}"
    );
    // The server's own words, not git's — git's stderr is locale-dependent
    // (this box answers in Chinese), so pinning that would make the test fail
    // on a different `LANG` for no behavioural reason.
    assert!(
        body.to_string().contains("is not inside a Git work tree"),
        "the 400 must come from the attached-workspace gate, not from some \
         other rejection that happens to share the status; body = {body}"
    );
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .len(),
        0
    );
}

/// `cwd: ""` is present (Some), not omitted. Empty string is not
/// absolute → 400, no track row. Distinct from missing/`null`.
#[tokio::test]
async fn post_api_tracks_empty_string_cwd_is_400() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-empty-cwd",
            "cwd": "",
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .len(),
        0
    );
}

/// Omitting cwd while sending `attach_folder: true` still cannot claim
/// `$HOME`. `into_parts` forces attach_folder false and `FolderClaim::Skip`.
#[tokio::test]
async fn post_api_tracks_omitted_cwd_ignores_attach_folder_true() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-omit-attach-true",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1, "track must land");
    assert_eq!(tracks[0].workspace.kind, TrackWorkspaceKind::Managed);
    assert_eq!(
        std::path::PathBuf::from(&tracks[0].workspace.path),
        boot.workspace_root
            .join(&boot.area_id)
            .join(tracks[0].id.as_str())
    );
    assert!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .is_empty(),
        "attach_folder true must claim nothing when cwd is omitted — there is \
         no user-pointed directory to claim"
    );
}

/// Non-absolute cwd → 400 before any DB write.
#[tokio::test]
async fn post_api_tracks_rejects_non_absolute_cwd() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.area_id,
            "title": "w-relative",
            "cwd": "relative/path",
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .area_folders_by_area(&boot.area_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

// ---------------------------------------------------------------------------
// Lifecycle → terminal_at stamping (track_update_tx)
// ---------------------------------------------------------------------------

/// Helper: create a fresh track in `Draft` state via the repo (bypassing
/// the route so we don't have to do the cwd/folder dance in every
/// lifecycle test).
async fn seed_track(repo: &Arc<dyn Repo>, area_id: &str) -> calm_server::model::Track {
    repo.track_create(calm_server::model::NewTrack {
        template_input: None,
        area_id: area_id.into(),
        title: "lifecycle-test".into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
}

/// Advance through `Draft → Planning → Dispatching → Working → Reviewing
/// → Done` via direct `track_update_tx` calls and assert that
/// `terminal_at` lands as `Some(_)` exactly once on the Done write.
#[tokio::test]
async fn lifecycle_to_done_stamps_terminal_at() {
    let boot = boot().await;
    let track = seed_track(&boot.repo, &boot.area_id).await;
    // Route everything through `track_update` (which opens a tx and
    // calls `track_update_tx` under the hood). The lifecycle validator
    // runs at the *route* layer; bypassing it here is fine — we're
    // isolating the terminal_at column write.

    // Each step uses the public `track_update` (which calls
    // `track_update_tx` under the hood). terminal_at must stay None
    // for every non-terminal transition and become Some on Done.
    for step in [
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Reviewing,
    ] {
        let updated = boot
            .repo
            .track_update(
                track.id.as_str(),
                TrackPatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            updated.terminal_at, None,
            "terminal_at must stay None while lifecycle is non-terminal ({step:?}); \
             updated row = {updated:?}",
        );
    }

    let before_done_ms = calm_server::model::now_ms();
    let done = boot
        .repo
        .track_update(
            track.id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let after_done_ms = calm_server::model::now_ms();

    let stamp = done
        .terminal_at
        .expect("terminal_at must be Some after lifecycle → Done");
    assert!(
        stamp >= before_done_ms && stamp <= after_done_ms,
        "terminal_at must be a unix-ms within the call window \
         (before={before_done_ms}, stamp={stamp}, after={after_done_ms})",
    );
    assert_eq!(done.lifecycle, TrackLifecycle::Done);
}

/// User-driven reopen (`Done → Planning`) must clear `terminal_at`.
#[tokio::test]
async fn lifecycle_reopen_clears_terminal_at() {
    let boot = boot().await;
    let track = seed_track(&boot.repo, &boot.area_id).await;

    // Force the track into Done first.
    for step in [
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Done,
    ] {
        boot.repo
            .track_update(
                track.id.as_str(),
                TrackPatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let done = boot
        .repo
        .track_get(track.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        done.terminal_at.is_some(),
        "preconditon: terminal_at stamped"
    );

    // Now reopen — terminal → planning is the only legal reopen edge.
    let reopened = boot
        .repo
        .track_update(
            track.id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Planning),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reopened.lifecycle, TrackLifecycle::Planning);
    assert_eq!(
        reopened.terminal_at, None,
        "reopen must clear terminal_at; got {reopened:?}",
    );
}

/// Working → Blocked is non-terminal; terminal_at must not be stamped.
#[tokio::test]
async fn lifecycle_working_to_blocked_leaves_terminal_at_unset() {
    let boot = boot().await;
    let track = seed_track(&boot.repo, &boot.area_id).await;

    for step in [
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Blocked,
    ] {
        let updated = boot
            .repo
            .track_update(
                track.id.as_str(),
                TrackPatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.terminal_at, None);
    }
}

/// Standalone tx surface check: `track_update` (which routes through
/// `track_update_tx`) lands `terminal_at = Some(_)` in the same write
/// as the lifecycle column. The route + MCP layers both call into
/// this same primitive, so a single repo-level assertion locks the
/// invariant down for every entry point.
#[tokio::test]
async fn track_update_tx_stamps_terminal_at_inside_one_tx() {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "tx-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = seed_track(&(repo.clone() as Arc<dyn Repo>), area.id.as_str()).await;
    let done = repo
        .track_update(
            track.id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        done.terminal_at.is_some(),
        "track_update_tx must stamp terminal_at when lifecycle lands in a terminal state; \
         got {done:?}"
    );
}

// ---------------------------------------------------------------------------
// GET /api/tracks window query
// ---------------------------------------------------------------------------

/// Three tracks with engineered timestamps cover every branch of the
/// window predicate `created_at <= until AND (terminal_at IS NULL OR
/// terminal_at >= since)`:
///
///   * A — created=1, terminal=2  → terminated *before* the window.
///   * B — created=5, terminal=NULL → open across the window.
///   * C — created=10, terminal=12 → created *after* the window.
///
/// Asking for `since=4, until=8` must include only B. The test forces
/// the timestamps via raw SQL after the kernel mints the rows (the
/// real `now_ms()` would make all three cluster within a millisecond
/// and the window math wouldn't be stable).
#[tokio::test]
async fn list_tracks_window_filters_by_created_and_terminal_at() {
    let boot = boot().await;
    let a = seed_track(&boot.repo, &boot.area_id).await;
    let b = seed_track(&boot.repo, &boot.area_id).await;
    let c = seed_track(&boot.repo, &boot.area_id).await;

    // Pin the timestamps via raw SQL. The kernel `track_create_tx`
    // / `track_update_tx` always stamp `now_ms()`; for the window
    // predicate test we need stable, separated values that the
    // boundary code never overwrites. Routing through the
    // `SqlxRepo::pool()` accessor keeps the test out of the public
    // trait surface — the production code path is unchanged.
    let pool = boot.sqlx_repo.pool();
    sqlx::query("UPDATE tracks SET created_at = ?1, terminal_at = ?2 WHERE id = ?3")
        .bind(1_i64)
        .bind(2_i64)
        .bind(a.id.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET created_at = ?1, terminal_at = NULL WHERE id = ?2")
        .bind(5_i64)
        .bind(b.id.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET created_at = ?1, terminal_at = ?2 WHERE id = ?3")
        .bind(10_i64)
        .bind(12_i64)
        .bind(c.id.as_str())
        .execute(pool)
        .await
        .unwrap();

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/tracks?since=4&until=8&area_id={}", boot.area_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body = {body}");
    let arr = body.as_array().expect("array body");
    let ids: Vec<String> = arr
        .iter()
        .map(|w| w.get("id").and_then(Value::as_str).unwrap().to_string())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "exactly one track (B) must match since=4&until=8; got ids={ids:?}",
    );
    assert_eq!(ids[0], b.id.to_string());
}

/// `since > until` is a 400.
#[tokio::test]
async fn list_tracks_window_inverted_returns_400() {
    let boot = boot().await;
    let (status, body) = get(boot.app.clone(), "/api/tracks?since=100&until=50").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
}

/// Empty query keeps ordinary tracks visible when no window filters apply.
#[tokio::test]
async fn list_tracks_window_no_params_keeps_ordinary_tracks_visible() {
    let boot = boot().await;
    seed_track(&boot.repo, &boot.area_id).await;
    seed_track(&boot.repo, &boot.area_id).await;
    seed_track(&boot.repo, &boot.other_area_id).await;

    let (status, body) = get(boot.app.clone(), "/api/tracks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(3));
}

/// `area_id` alone partitions by area.
#[tokio::test]
async fn list_tracks_window_area_id_filter() {
    let boot = boot().await;
    seed_track(&boot.repo, &boot.area_id).await;
    seed_track(&boot.repo, &boot.area_id).await;
    seed_track(&boot.repo, &boot.other_area_id).await;

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/tracks?area_id={}", boot.area_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(2));
}

/// INV-CHAT-005 paired route-boundary contract: NULL-purpose ordinary tracks
/// and launchpads remain visible in all three public list URIs, while only
/// area-chat is hidden. The repository still returns the full set.
#[tokio::test]
async fn public_track_lists_hide_only_area_chat_and_repo_keeps_full_set() {
    let boot = boot().await;
    let ordinary = seed_track(&boot.repo, &boot.area_id).await;
    let launchpad = seed_track(&boot.repo, &boot.area_id).await;
    let chat = seed_track(&boot.repo, &boot.area_id).await;
    sqlx::query("UPDATE tracks SET purpose = 'launchpad' WHERE id = ?1")
        .bind(launchpad.id.as_str())
        .execute(boot.sqlx_repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(chat.id.as_str())
        .execute(boot.sqlx_repo.pool())
        .await
        .unwrap();

    let expected = [ordinary.id.as_str(), launchpad.id.as_str()];
    // #1318 S2 (第一轮评审 F8) — the third URI is the bare list. The deleted
    // `template_tracks_are_hidden_from_lists_and_visible_by_id` walked all
    // three; this one covered only the two area-scoped ones, so retiring that
    // test would have dropped `GET /api/tracks` with no `area_id` from every
    // list-hiding assertion in the suite. It is the same handler and `area_id`
    // is only an optional query filter, so this is cheap coverage rather than a
    // new property — but "cheap" is not "already covered". Only tracks in
    // `boot.area_id` are seeded here, so the expected set is identical.
    for uri in [
        format!("/api/areas/{}/tracks", boot.area_id),
        format!("/api/tracks?area_id={}", boot.area_id),
        "/api/tracks".to_string(),
    ] {
        let (status, body) = get(boot.app.clone(), &uri).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let ids: Vec<_> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|track| track["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2, "{uri}: only chat is hidden; ids={ids:?}");
        for id in expected {
            assert!(ids.contains(&id), "{uri}: expected visible track {id}");
        }
        assert!(!ids.contains(&chat.id.as_str()), "{uri}: chat leaked");
    }

    let repo_tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(
        repo_tracks.len(),
        3,
        "repository readers require the full set"
    );
    assert!(repo_tracks.iter().any(|track| track.id == chat.id));

    let repo_window = boot
        .repo
        .tracks_window(Some(&boot.area_id), None, None)
        .await
        .unwrap();
    assert_eq!(
        repo_window.len(),
        3,
        "tracks_window must also retain the repository's full set"
    );
    assert!(repo_window.iter().any(|track| track.id == chat.id));
}

// ---------------------------------------------------------------------------
// Issue #275 — the two resolvers must agree
// ---------------------------------------------------------------------------

/// `GET /api/areas/resolve` and the `POST /api/tracks` owner scan are two
/// separate readers of the same claim table. They must pick the **same**
/// row for the same cwd, because the UI chains them: NewTaskForm resolves
/// the cwd, auto-selects the area it names, and posts the track with that
/// `area_id`. A resolver that disagrees turns that chain into a 409 on a
/// area the user never chose.
///
/// The claim rules make overlapping rows unreachable over HTTP, so this
/// test seeds them through the raw repo (`area_folder_create`, the
/// unchecked primitive) — the corrupt-DB state — and pins that even
/// *there* the two answers are identical. This is the case that regressed
/// when only one of the two scans dropped its longest-prefix tiebreak:
/// resolve said `/a` (area A) while track-create said `/a/b` (area B).
#[tokio::test]
async fn resolve_and_track_create_agree_on_overlapping_rows() {
    let boot = boot().await;

    // Corrupt state: two claims cover `/a/b/c`, under different areas.
    // `ORDER BY path ASC` puts `/a` first; `/a/b` is the longer prefix.
    // `ORDER BY path ASC` still puts the outer claim first: `<root>` is a
    // strict prefix of `<root>/b`, so it sorts before it, exactly as `/a`
    // sorted before `/a/b`.
    let outer = attached_repo_fixture("cwd-terminal-agree");
    let inner = attached_sub("cwd-terminal-agree", "b");
    let cwd = attached_sub("cwd-terminal-agree", "b/c");
    boot.repo
        .area_folder_create(&boot.area_id, &outer)
        .await
        .unwrap();
    boot.repo
        .area_folder_create(&boot.other_area_id, &inner)
        .await
        .unwrap();

    // Resolver 1 — the endpoint the frontend calls.
    let (status, body) = get(boot.app.clone(), &format!("/api/areas/resolve?path={cwd}")).await;
    assert_eq!(status, StatusCode::OK);
    let resolved_area = body["area_id"].as_str().unwrap().to_string();
    assert_eq!(
        body["folder_path"].as_str().unwrap(),
        outer,
        "shared find_owner takes the first row in ORDER BY path ASC"
    );
    assert_eq!(resolved_area, boot.area_id);

    // Resolver 2 — the track-create owner scan. Posting with exactly the
    // area `/api/areas/resolve` just named must be accepted.
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": resolved_area,
            "title": "w-agree",
            "cwd": cwd.clone(),
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    // The contract under test is "not a 409" — the two resolvers agree, so
    // the owner scan must not refuse the area the resolver just named. The
    // 201-or-500 tolerance below is only the file's stub-daemon convention.
    assert_ne!(
        status,
        StatusCode::CONFLICT,
        "track create must not refuse the area `/api/areas/resolve` named for this cwd; \
         body={body}"
    );
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "track create must accept the area `/api/areas/resolve` named for this cwd; \
         got {status} body={body}",
    );
    let tracks = boot.repo.tracks_by_area(&resolved_area).await.unwrap();
    assert_eq!(
        tracks.len(),
        1,
        "the track must have landed under that area"
    );
    assert_eq!(tracks[0].workspace.path, cwd);

    // ...and the area the resolver did NOT name must still be refused,
    // so "they agree" is not vacuously true by accepting everything.
    let (status, _body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({
            "area_id": boot.other_area_id,
            "title": "w-disagree",
            "cwd": cwd.clone(),
            "attach_folder": false,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the non-owning area must still 409"
    );
}
