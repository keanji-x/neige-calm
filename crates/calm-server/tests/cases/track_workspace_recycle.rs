//! Issue #1147 S5 — recycling a track workspace, driven through the real
//! `DELETE /api/tracks/{id}` and `DELETE /api/areas/{id}` routes.
//!
//! **These tests really delete.** That is the whole point. S4's red team
//! established that the assertion which actually stops the accident is not a
//! SQL query over `workspace_path` but "perform the deletion for real, then go
//! and look at whether the other repository is still there". Every guard below
//! is therefore stated as: run the production route, then read the filesystem.
//!
//! The four guards (design `docs/1147-workspace-design.md` §生命周期, and
//! `calm_server::workspace_recycle`'s module doc):
//!
//!   1. `kind == Managed`
//!   2. `canonicalize(path)` under `canonicalize(workspace_root)`
//!   3. `.git/neige-workspace` present and equal to this track's id
//!   4. the owning area is not the system area
//!
//! Each has a test here that fails if the guard is removed, and each such test
//! asserts on *bytes on disk*, not on a decision value.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    /// #1147 S3 — the registry `delete_track`'s teardown acts on.
    ///
    /// Held for the same reason the re-point suite holds it: the registry is
    /// populated naturally (creating a track registers a live planner-harness
    /// runtime), so `teardown_track_deletion`'s `harness.get`/`shutdown`/`remove`
    /// loop does run under test — but nothing ever inspected the slot
    /// afterwards, so removing the loop turned nothing red. Installing a
    /// harness under a **known** runtime id is what lets a test name it and
    /// assert it is gone. Measured on the re-point path; this path has the
    /// identical shape, so it is covered here too rather than left as the same
    /// latent gap.
    harness: calm_server::harness::HarnessRegistry,
    roles: CardRoleCache,
    tracks: TrackAreaCache,
    shared_codex: Arc<calm_server::shared_codex_appserver::SharedCodexAppServer>,
    tmp: TempDir,
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
    let shared_codex = SharedCodexAppServer::new_fake_running_with_pending(sqlx_repo.clone(), None);
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
    .with_workspace_root(workspace_root.clone());
    let harness = state.harness.clone();
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo: sqlx_repo,
        workspace_root,
        harness,
        roles,
        tracks,
        shared_codex,
        tmp,
    }
}

/// Install a real `PlannerHarness` in the registry under the track's live
/// planner-harness runtime, and return that runtime id. Twin of the helper in
/// `track_workspace_repoint.rs`; see `Boot::harness` for why it exists.
async fn install_live_harness(b: &Boot, track_id: &str) -> String {
    let runtime_id: String = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id LIMIT 1",
    )
    .bind(track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let card_id: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
        .bind(&runtime_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let repo: Arc<dyn Repo> = b.repo.clone();
    let (harness, _observations) = calm_server::harness::PlannerHarness::run_unstarted_for_test(
        calm_server::harness::PlannerHarnessParams {
            runtime_id: runtime_id.clone(),
            track_id: track_id.to_string().into(),
            card_id: card_id.into(),
            thread_id: None,
            repo,
            events: calm_server::event::EventBus::new(),
            card_role_cache: b.roles.clone(),
            track_area_cache: b.tracks.clone(),
            daemon: b.shared_codex.clone(),
            config: Default::default(),
            snapshot: calm_server::harness::HarnessSnapshot::initial(0, Vec::new()),
        },
        8,
    );
    b.harness.insert(runtime_id.clone(), harness);
    assert!(
        b.harness.get(&runtime_id).is_some(),
        "premise: harness live"
    );
    runtime_id
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

async fn create_track(b: &Boot, body: Value) -> Value {
    let (status, text) = request(b.app.clone(), "POST", "/api/tracks", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    serde_json::from_str(&text).unwrap()
}

async fn delete_track(b: &Boot, track_id: &str) -> (StatusCode, String) {
    request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{track_id}"),
        None,
    )
    .await
}

async fn delete_area(b: &Boot, area_id: &str) -> (StatusCode, String) {
    request(
        b.app.clone(),
        "DELETE",
        &format!("/api/areas/{area_id}"),
        None,
    )
    .await
}

async fn workspace_path(b: &Boot, track_id: &str) -> PathBuf {
    let path: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id=?1")
        .bind(track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    PathBuf::from(path)
}

/// The single managed track created by `POST /api/tracks` with only a title.
async fn managed_track(b: &Boot, area_id: &str, title: &str) -> (String, PathBuf) {
    let track = create_track(
        b,
        json!({"area_id": area_id, "title": title, "theme": theme()}),
    )
    .await;
    let id = track["id"].as_str().unwrap().to_string();
    let path = workspace_path(b, &id).await;
    assert!(
        path.join(".git").is_dir(),
        "expected a repository at {path:?}"
    );
    (id, path)
}

/// A user-owned git repository with a real working file, outside the managed
/// root. Attached tracks point at directories shaped like this.
fn user_repo(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "user"],
        vec!["config", "user.email", "user@example.com"],
        // #1147 S3 — keep git from touching this repository behind our back;
        // background maintenance after a commit leaves a lock file that a
        // fingerprint pair can straddle. Measured on CI (git 2.55), invisible
        // on a 2.39 host.
        vec!["config", "gc.auto", "0"],
        vec!["config", "maintenance.auto", "false"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(at)
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    std::fs::write(
        at.join(".git/info/exclude"),
        b"# user's own exclude\ntarget/\n",
    )
    .unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(["add", "-A"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(["commit", "-m", "user commit", "--no-verify"])
        .status()
        .unwrap();
    assert!(status.success());
    at.to_path_buf()
}

async fn attached_track(b: &Boot, area_id: &str, title: &str, path: &Path) -> String {
    let track = create_track(
        b,
        json!({
            "area_id": area_id,
            "title": title,
            "cwd": path.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    track["id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Byte-for-byte fingerprints
// ---------------------------------------------------------------------------

/// Every path under `root`, with file contents. Directories map to `None`,
/// symlinks to their target, files to their exact bytes.
///
/// Comparing this before and after a delete is the assertion the brief calls
/// for: not "the directory still exists", but "not one byte moved". It
/// deliberately includes `.git/` — `.git/info/exclude` growing a
/// `.claude/worktrees/` line, or a `neige-workspace` marker appearing, are both
/// ways the server could have taken ownership of a user's repository, and both
/// show up here as a diff.
fn fingerprint(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    /// Git's **own** transient lock files under `.git/`, by exact name.
    ///
    /// Measured on CI (git 2.55) and not reproducible on this host (git 2.39):
    /// background maintenance creates `.git/objects/maintenance.lock` after a
    /// commit and removes it moments later, so a before/after pair straddling
    /// that window reports `removed: …maintenance.lock` and blames the server
    /// for a file it never saw. `user_repo` also turns maintenance off; this is
    /// the half that does not depend on remembering to.
    ///
    /// **Named, and rooted at `.git/`, on purpose.** The first version matched
    /// any `*.lock` anywhere, which was strictly wrong: it blinded the
    /// fingerprint to `.git/config.lock` and `.git/index.lock` — the exact
    /// files `workspace_materialize::clear_our_stale_git_locks` deletes — so
    /// the one production routine that removes files from a repository became
    /// invisible to the assertion whose entire job is "the server did not touch
    /// the user's repository". It also hid a work-tree `Cargo.lock`. An
    /// unexpected `*.lock` must fail this assertion, not be waved through.
    fn is_transient_git_lock(rel: &Path) -> bool {
        const NAMES: [&str; 2] = ["maintenance.lock", "gc.pid.lock"];
        rel.starts_with(".git")
            && rel
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| NAMES.contains(&name))
    }
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if is_transient_git_lock(&rel) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(rel, Some(target.into_os_string().into_encoded_bytes()));
            } else if meta.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn diff(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, bytes) in before {
        match after.get(path) {
            None => out.push(format!("removed: {}", path.display())),
            Some(other) if other != bytes => out.push(format!("changed: {}", path.display())),
            Some(_) => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            out.push(format!("added: {}", path.display()));
        }
    }
    out
}

fn head(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn trash_entries(workspace_root: &Path) -> Vec<PathBuf> {
    let trash = workspace_root.join(".trash");
    let Ok(entries) = std::fs::read_dir(&trash) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    out.sort();
    out
}

fn trash_entry_for(workspace_root: &Path, track_id: &str) -> Option<PathBuf> {
    trash_entries(workspace_root).into_iter().find(|path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&format!("{track_id}-")))
    })
}

// ---------------------------------------------------------------------------
// The happy path — a managed workspace is reclaimed, by moving not deleting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_managed_track_moves_its_workspace_into_the_trash() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;
    std::fs::write(path.join("worker-output.txt"), b"generated").unwrap();
    let before = fingerprint(&path);

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        !path.exists(),
        "the managed workspace is still at its old path"
    );
    let trashed = trash_entry_for(&b.workspace_root, &track_id).unwrap_or_else(|| {
        panic!(
            "nothing in the trash: {:?}",
            trash_entries(&b.workspace_root)
        )
    });
    // Moved, not deleted: every byte is still readable under `.trash`. This is
    // what makes a future guard bug a leak rather than data loss.
    assert!(
        diff(&before, &fingerprint(&trashed)).is_empty(),
        "the trashed copy is not identical to what was recycled"
    );
    assert_eq!(
        std::fs::read(trashed.join("worker-output.txt")).unwrap(),
        b"generated"
    );
    // The `<root>/<area_id>/` layer deliberately stays: it is the namespace
    // for every future track in an area that still exists. It is reclaimed by
    // area deletion, not track deletion (design §生命周期).
    assert!(b.workspace_root.join(&area_id).is_dir());
}

// ---------------------------------------------------------------------------
// Guard 1 — `kind == Managed`
// ---------------------------------------------------------------------------

/// Deleting an **attached** track must not change a single byte of the user's
/// repository. Stated as a full recursive fingerprint rather than
/// `assert!(dir.exists())`, because the failure modes that matter here are
/// partial: a `.claude/worktrees/` line appended to the user's
/// `.git/info/exclude`, a `neige-workspace` marker dropped into their `.git/`,
/// a working file removed by an over-broad sweep.
///
/// **What this test does and does not isolate.** Measured by mutation: turning
/// guard 1 off leaves this test green, because a real attached repository is
/// also outside the managed root (guard 2) and also carries no ownership
/// marker (guard 3). That redundancy is the good news — three independent
/// things have to break before a user's repository moves — but it means the
/// single-violation fixture for guard 1 has to construct the one shape
/// production cannot: an `Attached` row inside the root with a valid marker.
/// That fixture is `workspace_recycle::tests::an_attached_workspace_is_refused`
/// in the unit suite, and guard 1 is the only reason it refuses.
#[tokio::test]
async fn deleting_an_attached_track_leaves_the_users_repository_byte_for_byte() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let repo = user_repo(&b.tmp.path().join("users-project"));
    let track_id = attached_track(&b, &area_id, "attached", &repo).await;

    let before = fingerprint(&repo);
    let before_head = head(&repo).expect("the user repo must have a HEAD to begin with");
    let before_exclude = std::fs::read(repo.join(".git/info/exclude")).unwrap();

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    let changes = diff(&before, &fingerprint(&repo));
    assert!(
        changes.is_empty(),
        "the user's repository changed: {changes:?}"
    );
    assert_eq!(head(&repo).as_deref(), Some(before_head.as_str()));
    let after_exclude = std::fs::read(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(
        after_exclude.len(),
        before_exclude.len(),
        "`.git/info/exclude` changed size: {:?} -> {:?}",
        String::from_utf8_lossy(&before_exclude),
        String::from_utf8_lossy(&after_exclude)
    );
    assert!(
        !repo.join(".git/neige-workspace").exists(),
        "an ownership marker was planted in the user's repository — with one \
         there, a later recycle would consider it ours"
    );
    assert!(
        trash_entry_for(&b.workspace_root, &track_id).is_none(),
        "an attached workspace was moved to the trash"
    );
}

// ---------------------------------------------------------------------------
// Guard 3 — the ownership marker, and that it names THIS track
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_managed_workspace_without_our_marker_is_left_on_disk() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;
    // Design gap N5 in the flesh: a partial restore, or a stray cleanup, took
    // the marker. We can no longer prove the directory is ours, so we do not
    // touch it — the row still goes away, so the track stays deletable.
    std::fs::remove_file(path.join(".git/neige-workspace")).unwrap();
    let before = fingerprint(&path);

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(diff(&before, &fingerprint(&path)).is_empty());
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_none());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE id=?1")
        .bind(&track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "refusing to delete the directory must not wedge the row"
    );
}

#[tokio::test]
async fn a_managed_workspace_whose_marker_names_another_track_is_left_on_disk() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;
    let (other_id, _) = managed_track(&b, &area_id, "neighbour").await;
    // The shape S4 exists to make unconstructible: a row pointing at another
    // track's managed directory. If it ever occurs again, the marker is the
    // thing that stops the delete.
    std::fs::write(path.join(".git/neige-workspace"), format!("{other_id}\n")).unwrap();
    let before = fingerprint(&path);

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        diff(&before, &fingerprint(&path)).is_empty(),
        "a directory whose marker names a different track was recycled"
    );
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_none());
}

// ---------------------------------------------------------------------------
// Guard 2 — canonical containment, not a lexical prefix
// ---------------------------------------------------------------------------

/// The stored path is lexically under the managed root; the bytes are not.
/// A `starts_with` check on the stored string passes and the user's repository
/// outside the root gets moved away.
#[tokio::test]
async fn a_symlinked_workspace_resolving_outside_the_root_is_left_on_disk() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;

    // Relocate the real repository outside the root and leave a symlink at the
    // stored path. The marker still names this track, so containment is the only
    // guard between the delete and the outside directory.
    let outside = b.tmp.path().join("outside-the-root");
    std::fs::rename(&path, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(
        path.starts_with(&b.workspace_root),
        "the stored path must still look contained to a lexical check"
    );
    let before = fingerprint(&outside);

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        diff(&before, &fingerprint(&outside)).is_empty(),
        "a lexical containment check let a directory outside the root be recycled"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
}

// ---------------------------------------------------------------------------
// Guard 4 — the system area, at both layers
// ---------------------------------------------------------------------------

/// The row layer. `DELETE /api/areas/{id}` already 403s a system area; this
/// route used to be the asymmetric one, deleting a system-area track row and
/// returning 204 while the directory (correctly, via guard 4) survived.
///
/// **That combination is the actual leak.** Reclaiming a managed directory
/// requires the track row that names it, so a deleted row makes its directory
/// unreachable forever — every launchpad delete + `ensure` cycle would strand
/// one more orphan repository under the managed root. The 403 closes it: the
/// row and the directory now agree.
#[tokio::test]
async fn a_system_area_track_cannot_be_deleted_through_the_public_route() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let ensured: Value = serde_json::from_str(&body).unwrap();
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    let path = workspace_path(&b, &track_id).await;
    assert!(
        path.join(".git").is_dir(),
        "the launchpad must be materialized, or this proves nothing"
    );
    let before = fingerprint(&path);

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");

    // Both halves, because either alone is the broken state: the row is what
    // keeps the directory reclaimable, and the directory is the kernel's.
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE id=?1")
        .bind(&track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "the launchpad track row was deleted");
    assert!(
        diff(&before, &fingerprint(&path)).is_empty(),
        "the kernel-owned launchpad workspace changed"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// Single-violation fixture for the 403 above: the *same* track, moved to a
/// user area, deletes fine. Without this, the assertion could be passing
/// because the launchpad is undeletable for some unrelated reason (a child
/// track, an in-flight forge action, a lifecycle state) rather than because it
/// is system-owned.
#[tokio::test]
async fn the_same_track_in_a_user_area_deletes_normally() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let ensured: Value = serde_json::from_str(&body).unwrap();
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    let path = workspace_path(&b, &track_id).await;
    assert_eq!(
        delete_track(&b, &track_id).await.0,
        StatusCode::FORBIDDEN,
        "precondition: it is refused while system-owned"
    );

    // Flip only the owning area's kind. Everything else about the track — its
    // cards, its workspace, its lifecycle — is untouched.
    let user_area = create_area(&b, "Atlas").await;
    sqlx::query("UPDATE tracks SET area_id=?1 WHERE id=?2")
        .bind(&user_area)
        .bind(&track_id)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the 403 is not about system ownership after all: body={body}"
    );
    // And with the row gone through a legitimate path, guard 4 no longer
    // applies either, so the directory is reclaimed rather than stranded.
    assert!(!path.exists());
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_some());
}

// ---------------------------------------------------------------------------
// Child tracks — S4's two shapes, from the recycling side
// ---------------------------------------------------------------------------

/// Parent `attached` ⇒ the child shares the parent's path (S4's amended D7).
/// Deleting the child must not touch it: guard 1 alone carries this, because
/// the child row is `attached` too.
#[tokio::test]
async fn deleting_a_child_of_an_attached_parent_leaves_the_shared_repository() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let repo = user_repo(&b.tmp.path().join("users-project"));
    let parent_id = attached_track(&b, &area_id, "attached parent", &repo).await;
    let child_id = support_child_track(&b, &parent_id).await;
    assert_eq!(
        workspace_path(&b, &child_id).await,
        repo,
        "S4: a child of an attached parent inherits the parent's path"
    );

    let before = fingerprint(&repo);
    let before_head = head(&repo).unwrap();
    let (status, body) = delete_track(&b, &child_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    let changes = diff(&before, &fingerprint(&repo));
    assert!(
        changes.is_empty(),
        "the shared parent repository changed: {changes:?}"
    );
    assert_eq!(
        head(&repo).as_deref(),
        Some(before_head.as_str()),
        "the parent repository's HEAD no longer resolves"
    );
}

/// Parent `managed` ⇒ the child owns a separate managed directory. Deleting
/// the child recycles that one and leaves the parent's repository working.
#[tokio::test]
async fn deleting_a_child_of_a_managed_parent_leaves_the_parent_repository() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (parent_id, parent_path) = managed_track(&b, &area_id, "managed parent").await;
    let child_id = support_child_track(&b, &parent_id).await;
    let child_path = workspace_path(&b, &child_id).await;
    assert_ne!(child_path, parent_path);

    let parent_before = fingerprint(&parent_path);
    let parent_head = head(&parent_path).unwrap();
    let (status, body) = delete_track(&b, &child_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        !child_path.exists(),
        "the child's own workspace was not recycled"
    );
    assert!(trash_entry_for(&b.workspace_root, &child_id).is_some());
    let changes = diff(&parent_before, &fingerprint(&parent_path));
    assert!(
        changes.is_empty(),
        "the parent repository changed: {changes:?}"
    );
    assert_eq!(head(&parent_path).as_deref(), Some(parent_head.as_str()));
}

/// A deletion only reduces tree membership and must remain possible when an
/// upgraded tree was already admission-frozen. The survivor's immutable work
/// remains, new admission stays frozen, and no fallible postcondition may run
/// after the victim's workspace has been recycled.
#[tokio::test]
async fn deleting_a_leaf_from_a_frozen_tree_commits_after_recycling_its_workspace() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (root_id, _) = managed_track(&b, &area_id, "root").await;
    let (victim_id, victim_path) = managed_track(&b, &area_id, "victim").await;
    let (survivor_id, _) = managed_track(&b, &area_id, "survivor").await;
    sqlx::query("UPDATE tracks SET created_at=0,tree_task_budget=1 WHERE id=?1")
        .bind(&root_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1,created_at=1 WHERE id=?2")
        .bind(&root_id)
        .bind(&victim_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1,created_at=2 WHERE id=?2")
        .bind(&root_id)
        .bind(&survivor_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) \
         VALUES('survivor:fixed',?1,'fixed','codex','fixed','{}','[]',0,'running','spec',0,0)",
    )
    .bind(&survivor_id)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, body) = delete_track(&b, &victim_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.repo.track_get(&victim_id).await.unwrap().is_none());
    assert!(!victim_path.exists());
    assert!(trash_entry_for(&b.workspace_root, &victim_id).is_some());
    assert_eq!(
        b.tracks
            .area_of(&calm_server::ids::TrackId::from(victim_id.clone())),
        None
    );
    let survivor_status: String =
        sqlx::query_scalar("SELECT status FROM tasks WHERE id='survivor:fixed'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(survivor_status, "running");
}

/// A malformed surviving report must fail before the victim's runtime or
/// workspace is touched. The compensation path remains a last-resort fence for
/// a concurrent failure after this preflight, not the ordinary error path.
#[tokio::test]
async fn invalid_survivor_report_fails_before_teardown_or_recycling() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (root_id, _) = managed_track(&b, &area_id, "root").await;
    let (victim_id, victim_path) = managed_track(&b, &area_id, "victim").await;
    let (survivor_id, _) = managed_track(&b, &area_id, "survivor").await;
    let runtime_id = install_live_harness(&b, &victim_id).await;
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id IN (?2,?3)")
        .bind(&root_id)
        .bind(&victim_id)
        .bind(&survivor_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    let corrupted =
        sqlx::query("UPDATE cards SET body_crdt=?1 WHERE track_id=?2 AND kind='track-report'")
            .bind(b"not-an-automerge-document".as_slice())
            .bind(&survivor_id)
            .execute(b.repo.pool())
            .await
            .unwrap()
            .rows_affected();
    assert_eq!(corrupted, 1);
    let before = fingerprint(&victim_path);

    let (status, body) = delete_track(&b, &victim_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert!(b.repo.track_get(&victim_id).await.unwrap().is_some());
    assert!(b.harness.get(&runtime_id).is_some());
    assert!(victim_path.exists());
    assert!(diff(&before, &fingerprint(&victim_path)).is_empty());
    assert!(trash_entry_for(&b.workspace_root, &victim_id).is_none());
    assert_eq!(
        b.tracks
            .area_of(&calm_server::ids::TrackId::from(victim_id.clone())),
        Some(area_id.into())
    );
}

/// The multi-stage delete is serialized per track. A concurrent loser must
/// observe the committed absence before it can move or compensate anything.
#[tokio::test]
async fn concurrent_deletes_cannot_resurrect_the_winners_workspace_or_cache_entry() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "one owner").await;
    let hook = calm_server::routes::tracks::TrackDeleteTeardownHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::tracks::install_track_delete_teardown_hook_for_test(
        &track_id,
        hook.clone(),
    );

    let first_app = b.app.clone();
    let first_id = track_id.clone();
    let first = tokio::spawn(async move {
        request(
            first_app,
            "DELETE",
            &format!("/api/tracks/{first_id}"),
            None,
        )
        .await
    });
    hook.entered.notified().await;
    let second_app = b.app.clone();
    let second_id = track_id.clone();
    let second = tokio::spawn(async move {
        request(
            second_app,
            "DELETE",
            &format!("/api/tracks/{second_id}"),
            None,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !second.is_finished(),
        "the second delete bypassed the per-track lock"
    );

    hook.release.notify_one();
    let (first_status, first_body) = first.await.unwrap();
    let (second_status, second_body) = second.await.unwrap();
    assert_eq!(first_status, StatusCode::NO_CONTENT, "body={first_body}");
    assert_eq!(second_status, StatusCode::NOT_FOUND, "body={second_body}");
    assert!(b.repo.track_get(&track_id).await.unwrap().is_none());
    assert!(!path.exists());
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_some());
    assert_eq!(
        b.tracks.area_of(&calm_server::ids::TrackId::from(track_id)),
        None
    );
}

/// Once the managed workspace has moved, dropping the HTTP request must not
/// drop the only owner of the remaining database commit and compensation.
#[tokio::test]
async fn canceling_the_request_after_recycle_still_converges_the_delete_saga() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "cancel-safe").await;
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track_id, hook.clone());

    let app = b.app.clone();
    let id = track_id.clone();
    let request_task =
        tokio::spawn(
            async move { request(app, "DELETE", &format!("/api/tracks/{id}"), None).await },
        );
    hook.entered.notified().await;
    assert!(!path.exists(), "the workspace must already be recycled");

    request_task.abort();
    assert!(request_task.await.unwrap_err().is_cancelled());
    hook.release.notify_one();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if b.repo.track_get(&track_id).await.unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the detached delete saga must finish after request cancellation");
    assert!(!path.exists());
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_some());
    assert_eq!(
        b.tracks.area_of(&calm_server::ids::TrackId::from(track_id)),
        None
    );
}

/// The delete fence is shared with harness installation, not merely with a
/// second DELETE. A reset that starts after the teardown snapshot must wait;
/// otherwise it can install a runtime the snapshot never knew to stop.
#[tokio::test]
async fn planner_reset_cannot_install_a_harness_behind_track_deletion() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "one lifecycle").await;
    let runtime_id = install_live_harness(&b, &track_id).await;
    let card_id: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
        .bind(&runtime_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let hook = calm_server::routes::tracks::TrackDeleteTeardownHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::tracks::install_track_delete_teardown_hook_for_test(
        &track_id,
        hook.clone(),
    );

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

    let reset_app = b.app.clone();
    let reset_task = tokio::spawn(async move {
        request(
            reset_app,
            "POST",
            &format!("/api/cards/{card_id}/planner/reset"),
            None,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !reset_task.is_finished(),
        "planner reset bypassed the track lifecycle fence"
    );

    hook.release.notify_one();
    let (delete_status, delete_body) = delete_task.await.unwrap();
    assert_eq!(delete_status, StatusCode::NO_CONTENT, "body={delete_body}");
    let (reset_status, _) = tokio::time::timeout(std::time::Duration::from_secs(2), reset_task)
        .await
        .expect("reset must observe the completed deletion")
        .unwrap();
    assert!(!reset_status.is_success());
    assert!(b.repo.track_get(&track_id).await.unwrap().is_none());
    assert!(!path.exists());
    assert_eq!(
        b.harness.len_active(),
        0,
        "no harness may outlive its track"
    );
}

/// Area deletion uses the same operation fence as a track saga. It cannot
/// erase ownership rows while a failed track transaction is deciding whether
/// to restore its recycled workspace.
#[tokio::test]
async fn area_delete_waits_for_track_delete_compensation_to_finish() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (root_id, _) = managed_track(&b, &area_id, "root").await;
    let (victim_id, victim_path) = managed_track(&b, &area_id, "victim").await;
    let (survivor_id, _) = managed_track(&b, &area_id, "survivor").await;
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id IN (?2,?3)")
        .bind(&root_id)
        .bind(&victim_id)
        .bind(&survivor_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(
        &victim_id,
        hook.clone(),
    );

    let track_app = b.app.clone();
    let delete_id = victim_id.clone();
    let track_delete = tokio::spawn(async move {
        request(
            track_app,
            "DELETE",
            &format!("/api/tracks/{delete_id}"),
            None,
        )
        .await
    });
    hook.entered.notified().await;
    assert!(!victim_path.exists(), "victim must already be recycled");

    let corrupted =
        sqlx::query("UPDATE cards SET body_crdt=?1 WHERE track_id=?2 AND kind='track-report'")
            .bind(b"not-an-automerge-document".as_slice())
            .bind(&survivor_id)
            .execute(b.repo.pool())
            .await
            .unwrap()
            .rows_affected();
    assert_eq!(corrupted, 1);

    let area_app = b.app.clone();
    let delete_area_id = area_id.clone();
    let area_delete = tokio::spawn(async move {
        request(
            area_app,
            "DELETE",
            &format!("/api/areas/{delete_area_id}"),
            None,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !area_delete.is_finished(),
        "area deletion bypassed the in-flight track saga"
    );

    hook.release.notify_one();
    let (track_status, _) = track_delete.await.unwrap();
    assert_eq!(track_status, StatusCode::INTERNAL_SERVER_ERROR);
    let (area_status, area_body) = area_delete.await.unwrap();
    assert_eq!(area_status, StatusCode::NO_CONTENT, "body={area_body}");
    assert!(b.repo.track_get(&victim_id).await.unwrap().is_none());
    assert!(!victim_path.exists());
    assert!(trash_entry_for(&b.workspace_root, &victim_id).is_some());
    assert_eq!(
        b.tracks
            .area_of(&calm_server::ids::TrackId::from(victim_id)),
        None
    );
}

/// Drive the production child-track creation path. Copied in shape from
/// `today_launchpad.rs`: the parent task row is seeded directly because the
/// adapter only reads frozen task fields from it, while every decision about
/// the child's workspace runs in production code.
async fn support_child_track(b: &Boot, parent_track_id: &str) -> String {
    use calm_server::operation::child_track_adapter::{
        ChildTrackAdapter, ChildTrackOperationPayload,
    };
    use calm_server::operation::{Operation, Phase, ProviderAdapter};

    let task_id = format!("{parent_track_id}:child");
    let now = calm_server::model::now_ms();
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,acceptance_criteria,\
         depends_on_json,priority,status,declared_by,spawn,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'child','codex','child goal','{}','done','[]',0,'dispatched',\
         'spec','sub-wave',?3,?3)",
    )
    .bind(&task_id)
    .bind(parent_track_id)
    .bind(now)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let payload = serde_json::to_value(ChildTrackOperationPayload {
        task_id: task_id.clone(),
        parent_track_id: parent_track_id.into(),
        goal: "child goal".into(),
        acceptance: Some("done".into()),
        context: json!({}),
        cwd: None,
    })
    .unwrap();
    let operation = Operation {
        id: "op-child".into(),
        operation_key: task_id.clone(),
        kind: "child-track".into(),
        idempotency_key: Some(task_id.clone()),
        payload_hash: "test-hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({"type": "unknown", "id": null}),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let adapter = ChildTrackAdapter::new(
        b.repo.card_role_cache().clone(),
        b.repo.track_area_cache().clone(),
        b.workspace_root.clone(),
    );
    let mut tx = b.repo.pool().begin().await.unwrap();
    let output = adapter
        .prepare_tx(&mut tx, &payload, &operation)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    output.data["child_track_id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Area deletion
// ---------------------------------------------------------------------------

/// Before this slice, `DELETE /api/areas/{id}` left every managed repository
/// under the area on disk with no row pointing at it. Now the managed ones are
/// recycled and the attached one is not touched at all.
#[tokio::test]
async fn deleting_a_area_recycles_its_managed_workspaces_and_spares_attached_ones() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (first_id, first_path) = managed_track(&b, &area_id, "one").await;
    let (second_id, second_path) = managed_track(&b, &area_id, "two").await;
    let repo = user_repo(&b.tmp.path().join("users-project"));
    let attached_id = attached_track(&b, &area_id, "attached", &repo).await;
    let repo_before = fingerprint(&repo);

    let (status, body) = delete_area(&b, &area_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(!first_path.exists());
    assert!(!second_path.exists());
    assert!(trash_entry_for(&b.workspace_root, &first_id).is_some());
    assert!(trash_entry_for(&b.workspace_root, &second_id).is_some());
    assert!(
        trash_entry_for(&b.workspace_root, &attached_id).is_none(),
        "the attached track's directory was recycled"
    );
    let changes = diff(&repo_before, &fingerprint(&repo));
    assert!(
        changes.is_empty(),
        "the user's repository changed: {changes:?}"
    );
    // The `<root>/<area_id>/` layer goes too — that is the orphan tree this
    // route used to leave behind.
    assert!(
        !b.workspace_root.join(&area_id).exists(),
        "the area directory survived: {:?}",
        std::fs::read_dir(b.workspace_root.join(&area_id))
            .map(|d| d.map(|e| e.unwrap().path()).collect::<Vec<_>>())
    );
}

/// An area holding one recyclable and one un-provable workspace: the provable
/// one goes, the other stays, and so does the area directory that contains it.
/// `remove_dir` is non-recursive precisely so this cannot come out any other
/// way.
#[tokio::test]
async fn a_area_directory_with_an_unrecyclable_track_survives() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (good_id, good_path) = managed_track(&b, &area_id, "one").await;
    let (_bad_id, bad_path) = managed_track(&b, &area_id, "two").await;
    std::fs::remove_file(bad_path.join(".git/neige-workspace")).unwrap();
    let bad_before = fingerprint(&bad_path);

    let (status, body) = delete_area(&b, &area_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(!good_path.exists());
    assert!(trash_entry_for(&b.workspace_root, &good_id).is_some());
    assert!(diff(&bad_before, &fingerprint(&bad_path)).is_empty());
    assert!(b.workspace_root.join(&area_id).is_dir());
}

// ---------------------------------------------------------------------------
// Trash GC, through the routes
// ---------------------------------------------------------------------------

/// The retention policy is time-based and swept on each recycle. Proven end to
/// end: an entry stamped beyond the window is gone after the next delete, an
/// entry inside the window survives it.
#[tokio::test]
async fn the_trash_gc_expires_old_entries_on_the_next_delete() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (first_id, _) = managed_track(&b, &area_id, "one").await;
    let (second_id, _) = managed_track(&b, &area_id, "two").await;

    let (status, _) = delete_track(&b, &first_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let fresh = trash_entry_for(&b.workspace_root, &first_id).unwrap();

    // Plant an entry stamped well outside the retention window. Its name is the
    // only thing that dates it — the GC deliberately does not read mtime.
    let stale_stamp =
        calm_server::model::now_ms() - calm_server::workspace_recycle::TRASH_RETENTION_MS - 1;
    let stale = b
        .workspace_root
        .join(".trash")
        .join(format!("track-ancient-{stale_stamp}"));
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("payload"), b"old").unwrap();

    let (status, _) = delete_track(&b, &second_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(!stale.exists(), "the expired trash entry was not swept");
    assert!(fresh.exists(), "a fresh trash entry was swept early");
    assert!(trash_entry_for(&b.workspace_root, &second_id).is_some());
}

/// `DELETE /api/tracks/{id}` must take the track's live planner harness out of the
/// registry before it moves the directory.
///
/// Same shape, same latent gap as the re-point path, and the gap is an absent
/// **assertion** rather than absent execution — a probe showed the loop runs in
/// tests that install nothing, because creating a track registers a live
/// planner-harness runtime by itself. Nothing checked the slot afterwards, so
/// deleting `teardown_track_deletion`'s `harness.get` → `shutdown` → `remove`
/// turned nothing red. A surviving harness is a live run loop whose process cwd
/// follows the inode — it keeps writing into the directory after it has been
/// renamed into `.trash`, until the GC erases the lot.
///
/// Running a subset of this suite locally: pass `--no-fail-fast`, or a stop at
/// the first failure will under-report which tests a mutation actually kills.
#[tokio::test]
async fn deleting_a_track_takes_its_live_harness_out_of_the_registry() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let runtime_id = install_live_harness(&b, &track).await;

    let (status, body) = delete_track(&b, &track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        b.harness.get(&runtime_id).is_none(),
        "runtime {runtime_id} is still live in the registry after the track was \
         deleted; its run loop keeps writing into the directory that just moved \
         to the trash"
    );
    assert!(!path.exists(), "the workspace should have been recycled");
}
