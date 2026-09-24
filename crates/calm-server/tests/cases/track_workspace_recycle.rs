//! Recycling a track workspace through the real `DELETE /api/tracks/{id}` and `DELETE /api/areas/{id}` routes.
//! These tests really delete and assert on bytes on disk.

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

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    /// The registry `delete_track`'s teardown acts on; a harness under a known runtime id lets a test assert it is gone.
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

/// Install a real `PlannerHarness` under the track's live planner-harness runtime and return that runtime id.
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
            worker_session_id: runtime_id.clone(),
            track_id: track_id.to_string().into(),
            card_id: card_id.into(),
            thread_id: None,
            repo,
            events: calm_server::event::EventBus::new(),
            card_role_cache: b.roles.clone(),
            track_area_cache: b.tracks.clone(),
            backend: b.shared_codex.clone().into(),
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
        json!({"planner_provider": "codex", "area_id": area_id, "title": title, "theme": theme()}),
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
        // Keep git from touching this repository behind our back: background maintenance leaves a lock file a fingerprint pair can straddle.
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
            "planner_provider": "codex",
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

/// Every path under `root`, with file contents. Directories map to `None`, symlinks to their target, files to their exact bytes; `.git/` included.
fn fingerprint(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    /// Git's own transient lock files under `.git/`, by exact name; a broader `*.lock` match would blind the fingerprint to `config.lock`/`index.lock`.
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
    // The `<root>/<area_id>/` layer stays: it is reclaimed by area deletion, not track deletion.
    assert!(b.workspace_root.join(&area_id).is_dir());
}

/// Turning guard 1 off alone leaves this green (guards 2 and 3 also hold for a real attached repo); the single-violation fixture is in the unit suite.
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

#[tokio::test]
async fn a_managed_workspace_without_our_marker_is_left_on_disk() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;
    // The marker is gone, so the directory cannot be proven ours and is left alone; the row still goes away.
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
    // A row pointing at another track's managed directory: the marker is what stops the delete.
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

/// The stored path is lexically under the managed root; the bytes are not.
#[tokio::test]
async fn a_symlinked_workspace_resolving_outside_the_root_is_left_on_disk() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "research").await;

    // The marker still names this track, so containment is the only guard between the delete and the outside directory.
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

/// Reclaiming a managed directory requires the track row that names it, so deleting a system-area row would strand its directory.
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

/// Control for the 403 above: the same track, moved to a user area, deletes fine.
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
    assert!(!path.exists());
    assert!(trash_entry_for(&b.workspace_root, &track_id).is_some());
}

/// Parent `attached` ⇒ the child shares the parent's path; guard 1 alone carries this.
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

#[tokio::test]
async fn invalid_survivor_report_fails_before_teardown_or_recycling() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (root_id, _) = managed_track(&b, &area_id, "root").await;
    let (victim_id, victim_path) = managed_track(&b, &area_id, "victim").await;
    let (survivor_id, _) = managed_track(&b, &area_id, "survivor").await;
    let worker_session_id = install_live_harness(&b, &victim_id).await;
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
    assert!(b.harness.get(&worker_session_id).is_some());
    assert!(victim_path.exists());
    assert!(diff(&before, &fingerprint(&victim_path)).is_empty());
    assert!(trash_entry_for(&b.workspace_root, &victim_id).is_none());
    assert_eq!(
        b.tracks
            .area_of(&calm_server::ids::TrackId::from(victim_id.clone())),
        Some(area_id.into())
    );
}

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

#[tokio::test]
async fn canceling_the_request_after_recycle_still_converges_the_delete_saga() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "cancel-safe").await;
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: false,
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

#[tokio::test]
async fn panicking_track_commit_restores_the_owned_workspace_and_cache() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "panic-safe").await;
    let worker_session_id: String = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let before = fingerprint(&path);
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: true,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert!(diff(&before, &fingerprint(&path)).is_empty());
    assert_eq!(
        b.tracks.area_of(&calm_server::ids::TrackId::from(track_id)),
        Some(area_id.into())
    );
    assert!(
        b.harness.get(&worker_session_id).is_some(),
        "aborted track deletion did not recover its planner harness"
    );
}

#[tokio::test]
async fn planner_reset_cannot_install_a_harness_behind_track_deletion() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "one lifecycle").await;
    let worker_session_id = install_live_harness(&b, &track_id).await;
    let card_id: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
        .bind(&worker_session_id)
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

/// A failed DB status does not prove the in-memory run loop exited.
#[tokio::test]
async fn deletion_shuts_down_a_live_harness_even_when_its_session_is_failed() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, _) = managed_track(&b, &area_id, "failed but live").await;
    let worker_session_id = install_live_harness(&b, &track_id).await;
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
        .bind(&worker_session_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    assert!(b.harness.get(&worker_session_id).is_some());

    let (status, body) = delete_track(&b, &track_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(b.harness.get(&worker_session_id).is_none());
}

#[tokio::test]
async fn deletion_interrupts_a_turn_whose_start_response_arrives_late() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, _) = managed_track(&b, &area_id, "late turn").await;
    let (worker_session_id, thread_id): (String, String) = sqlx::query_as(
        "SELECT id,thread_id FROM worker_sessions WHERE track_id=?1 \
         AND thread_id IS NOT NULL ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let harness = b
        .harness
        .get(&worker_session_id)
        .expect("create must install the planner harness");
    let hook = calm_server::shared_codex_appserver::TurnStartReturnHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    b.shared_codex
        .install_turn_start_return_hook_for_test(hook.clone());
    harness
        .observe(calm_server::harness::Observation::TrackGoal {
            text: "issue now".into(),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), hook.entered.notified())
        .await
        .expect("turn/start must reach the delayed-return hook");

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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !delete_task.is_finished(),
        "delete must wait for in-flight turn issuance"
    );

    hook.release.notify_one();
    let (status, body) = delete_task.await.unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    assert!(
        b.shared_codex
            .interrupted_turns_for_test()
            .iter()
            .any(|(thread, turn)| thread == &thread_id && turn.starts_with("fake-turn-")),
        "the late accepted turn was not interrupted"
    );
    assert!(
        b.shared_codex
            .active_turn_id_for_thread(&thread_id)
            .is_none()
    );
    assert!(b.harness.get(&worker_session_id).is_none());
}

#[tokio::test]
async fn failed_late_turn_interrupt_aborts_delete_and_releases_pre_recycle_seals() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "late turn failure").await;
    let (worker_session_id, thread_id): (String, String) = sqlx::query_as(
        "SELECT id,thread_id FROM worker_sessions WHERE track_id=?1 \
         AND thread_id IS NOT NULL ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let harness = b.harness.get(&worker_session_id).expect("planner harness");
    let hook = calm_server::shared_codex_appserver::TurnStartReturnHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    b.shared_codex
        .install_turn_start_return_hook_for_test(hook.clone());
    b.shared_codex.fail_turn_interrupt_for_test(true);
    harness
        .observe(calm_server::harness::Observation::TrackGoal {
            text: "issue now".into(),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), hook.entered.notified())
        .await
        .expect("turn/start must reach the delayed-return hook");

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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    hook.release.notify_one();

    let (status, _) = delete_task.await.unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert!(
        path.exists(),
        "failed quiesce must not recycle the workspace"
    );
    assert!(
        b.shared_codex
            .active_turn_id_for_thread(&thread_id)
            .is_some(),
        "failed interrupt must retain the late turn id for retry"
    );
    assert!(
        !b.shared_codex.turn_thread_is_sealed_for_test(&thread_id),
        "pre-recycle failure must roll back the deletion seal"
    );
}

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
        panic_after_release: false,
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

#[tokio::test]
async fn canceling_the_request_after_area_recycle_still_finishes_the_owned_saga() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "owned area saga").await;
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: false,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());

    let app = b.app.clone();
    let delete_id = area_id.clone();
    let request_task = tokio::spawn(async move {
        request(app, "DELETE", &format!("/api/areas/{delete_id}"), None).await
    });
    hook.entered.notified().await;
    assert!(!path.exists(), "workspace must already be recycled");

    request_task.abort();
    assert!(request_task.await.unwrap_err().is_cancelled());
    hook.release.notify_one();

    let track_id = calm_server::ids::TrackId::from(track_id);
    let finished = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let area_is_gone = b.repo.area_get(&area_id).await.unwrap().is_none();
            let track_is_gone = b.repo.track_get(track_id.as_str()).await.unwrap().is_none();
            let cache_is_clear = b.tracks.area_of(&track_id).is_none();
            if area_is_gone && track_is_gone && cache_is_clear && !path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if finished.is_err() {
        panic!(
            "detached area deletion did not converge after request cancellation: \
             area_present={}, track_present={}, cached_area={:?}, workspace_exists={}",
            b.repo.area_get(&area_id).await.unwrap().is_some(),
            b.repo.track_get(track_id.as_str()).await.unwrap().is_some(),
            b.tracks.area_of(&track_id),
            path.exists(),
        );
    }
    assert!(b.repo.track_get(track_id.as_str()).await.unwrap().is_none());
    assert!(!path.exists());
    assert_eq!(b.tracks.area_of(&track_id), None);
}

#[tokio::test]
async fn first_message_track_create_cannot_commit_behind_an_area_deletion_snapshot() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let _ = managed_track(&b, &area_id, "existing").await;
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: false,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());

    let delete_app = b.app.clone();
    let delete_id = area_id.clone();
    let area_delete = tokio::spawn(async move {
        request(
            delete_app,
            "DELETE",
            &format!("/api/areas/{delete_id}"),
            None,
        )
        .await
    });
    hook.entered.notified().await;

    let create_app = b.app.clone();
    let create_area_id = area_id.clone();
    let track_create = tokio::spawn(async move {
        let request = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("content-type", "application/json")
            .header("idempotency-key", "area-delete-race")
            .body(Body::from(
                json!({
                "planner_provider": "codex",
                "area_id": create_area_id,
                "title": "too late",
                "first_message": "start after create",
                "theme": theme(),
                })
                .to_string(),
            ))
            .unwrap();
        let response = create_app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !track_create.is_finished(),
        "track creation bypassed the area deletion fence"
    );

    hook.release.notify_one();
    let (delete_status, delete_body) = area_delete.await.unwrap();
    assert_eq!(delete_status, StatusCode::NO_CONTENT, "body={delete_body}");
    let (create_status, _) = track_create.await.unwrap();
    assert_eq!(create_status, StatusCode::NOT_FOUND);
    assert!(b.repo.tracks_by_area(&area_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn area_commit_failure_restores_every_recycled_workspace() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (first_id, first_path) = managed_track(&b, &area_id, "first").await;
    let (second_id, second_path) = managed_track(&b, &area_id, "second").await;
    let first_before = fingerprint(&first_path);
    let second_before = fingerprint(&second_path);
    let active_before = b.harness.len_active();
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: true,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = delete_area(&b, &area_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.repo.area_get(&area_id).await.unwrap().is_some());
    assert!(b.repo.track_get(&first_id).await.unwrap().is_some());
    assert!(b.repo.track_get(&second_id).await.unwrap().is_some());
    assert!(diff(&first_before, &fingerprint(&first_path)).is_empty());
    assert!(diff(&second_before, &fingerprint(&second_path)).is_empty());
    assert_eq!(
        b.tracks.area_of(&calm_server::ids::TrackId::from(first_id)),
        Some(area_id.clone().into())
    );
    assert_eq!(
        b.tracks
            .area_of(&calm_server::ids::TrackId::from(second_id)),
        Some(area_id.into())
    );
    assert_eq!(
        b.harness.len_active(),
        active_before,
        "aborted area deletion did not recover all planner harnesses"
    );
}

#[tokio::test]
async fn failed_area_workspace_restore_keeps_the_surviving_thread_sealed() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "restore blocked").await;
    let (worker_session_id, thread_id): (String, String) = sqlx::query_as(
        "SELECT id,thread_id FROM worker_sessions WHERE track_id=?1 AND thread_id IS NOT NULL LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: true,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());
    let app = b.app.clone();
    let delete_id = area_id.clone();
    let deletion = tokio::spawn(async move {
        request(app, "DELETE", &format!("/api/areas/{delete_id}"), None).await
    });
    hook.entered.notified().await;
    assert!(!path.exists());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("occupied"), b"not the restored workspace").unwrap();
    hook.release.notify_one();

    let (status, body) = deletion.await.unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert!(b.repo.area_get(&area_id).await.unwrap().is_some());
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert!(
        b.shared_codex.turn_thread_is_sealed_for_test(&thread_id),
        "a surviving runtime must not resume against an unrestored workspace"
    );
    assert_eq!(
        b.harness.len_active(),
        0,
        "unsafe sealed runtime must not be recovered"
    );
    // Process restart loses the in-memory seal; the managed ownership marker is the durable quarantine.
    let runtime = b
        .repo
        .session_projection_by_id(&worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let reboot_registry = calm_server::harness::HarnessRegistry::new();
    let repo: Arc<dyn Repo> = b.repo.clone();
    let reboot_daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let outcome = calm_server::harness::spawn_recovered_harness(
        repo,
        EventBus::new(),
        b.roles.clone(),
        b.tracks.clone(),
        reboot_daemon,
        &reboot_registry,
        &calm_server::harness::new_track_delete_locks(),
        runtime,
        calm_server::harness::ClaimMode::Replace,
    )
    .await
    .unwrap();
    assert!(
        outcome.installed().is_none(),
        "boot recovery ignored the durable managed-workspace quarantine"
    );
}

#[tokio::test]
async fn panicking_area_commit_restores_every_owned_workspace() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let (track_id, path) = managed_track(&b, &area_id, "panic-safe area").await;
    let before = fingerprint(&path);
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: false,
        panic_after_release: true,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = delete_area(&b, &area_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.repo.area_get(&area_id).await.unwrap().is_some());
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert!(diff(&before, &fingerprint(&path)).is_empty());
    assert_eq!(
        b.tracks.area_of(&calm_server::ids::TrackId::from(track_id)),
        Some(area_id.into())
    );
}

/// Drive the production child-track creation path; the parent task row is seeded directly because the adapter only reads frozen fields from it.
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
    assert!(
        !b.workspace_root.join(&area_id).exists(),
        "the area directory survived: {:?}",
        std::fs::read_dir(b.workspace_root.join(&area_id))
            .map(|d| d.map(|e| e.unwrap().path()).collect::<Vec<_>>())
    );
}

/// `remove_dir` is non-recursive, so the area directory holding the un-provable workspace must survive.
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

/// A surviving harness is a live run loop whose cwd follows the inode: it keeps writing into the directory after it is renamed into `.trash`.
#[tokio::test]
async fn deleting_a_track_takes_its_live_harness_out_of_the_registry() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let worker_session_id = install_live_harness(&b, &track).await;

    let (status, body) = delete_track(&b, &track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert!(
        b.harness.get(&worker_session_id).is_none(),
        "runtime {worker_session_id} is still live in the registry after the track was \
         deleted; its run loop keeps writing into the directory that just moved \
         to the trash"
    );
    assert!(!path.exists(), "the workspace should have been recycled");
}
