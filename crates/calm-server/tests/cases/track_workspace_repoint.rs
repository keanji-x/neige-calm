//! Changing a track's workspace through the real `PATCH /api/tracks/{id}`; every test runs the route and then looks at the filesystem.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRef;
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
    /// The registry the route's in-memory fence acts on; tests install a real `PlannerHarness` into it.
    harness: calm_server::harness::HarnessRegistry,
    /// Kept so a test can call an operation directly, for guards no HTTP request can reach.
    state: AppState,
    roles: CardRoleCache,
    tracks: TrackAreaCache,
    shared_codex: Arc<calm_server::shared_codex_appserver::SharedCodexAppServer>,
    #[allow(dead_code)]
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
    let state_for_tests = state.clone();
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
        state: state_for_tests,
        roles,
        tracks,
        shared_codex,
        tmp,
    }
}

/// Install a real `PlannerHarness` under the track's live planner-harness runtime and return that runtime id; `run_unstarted_for_test` spawns no run loop.
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
            daemon: b.shared_codex.clone(),
            config: Default::default(),
            snapshot: calm_server::harness::HarnessSnapshot::initial(0, Vec::new()),
        },
        8,
    );
    b.harness.insert(runtime_id.clone(), harness);
    assert!(
        b.harness.get(&runtime_id).is_some(),
        "premise: the registry now holds a live harness for {runtime_id}"
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

/// A managed track: the title-only create the new FE sends.
async fn managed_track(b: &Boot, area_id: &str, title: &str) -> (String, PathBuf) {
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({"area_id": area_id, "title": title, "theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let track: Value = serde_json::from_str(&text).unwrap();
    let id = track["id"].as_str().unwrap().to_string();
    let path = workspace_path(b, &id).await;
    assert!(
        path.join(".git").is_dir(),
        "expected a materialized repository at {path:?}"
    );
    (id, path)
}

async fn workspace_path(b: &Boot, track_id: &str) -> PathBuf {
    let path: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id=?1")
        .bind(track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    PathBuf::from(path)
}

async fn workspace_row(b: &Boot, track_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
    )
    .bind(track_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap()
}

/// `PATCH /api/tracks/{id}` pointing a track at an existing repository.
async fn repoint_to(b: &Boot, track_id: &str, path: &Path) -> (StatusCode, String) {
    request(
        b.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track_id}"),
        Some(json!({"workspace": {
            "kind": "attached",
            "path": path.to_string_lossy(),
            "attach_folder": true,
        }})),
    )
    .await
}

/// A valid target, so a refusal cannot be coming from target validation.
async fn repoint(b: &Boot, track_id: &str) -> (StatusCode, String) {
    let target = user_repo(&b.tmp.path().join(format!("target-{track_id}")));
    repoint_to(b, track_id, &target).await
}

/// Every `planner-harness-start` payload submitted so far, oldest first.
async fn harness_start_payloads(b: &Boot) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind='planner-harness-start' \
         ORDER BY created_at_ms, id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap()
}

fn trash_entries(workspace_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace_root.join(".trash")) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    out.sort();
    out
}

fn git(at: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} in {at:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn with_identity(at: &Path) {
    git(at, &["config", "user.name", "fixture"]);
    git(at, &["config", "user.email", "fixture@example.com"]);
}

fn commit_count(path: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-list", "--count", "--all"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

/// A user-owned repository outside the managed root.
fn user_repo(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-b", "main"]);
    with_identity(at);
    // Keep git from touching this repository behind our back: background maintenance leaves a lock file a fingerprint pair can straddle.
    git(at, &["config", "gc.auto", "0"]);
    git(at, &["config", "maintenance.auto", "false"]);
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-q", "--no-verify", "-m", "user commit"]);
    at.to_path_buf()
}

#[tokio::test]
async fn a_pristine_track_is_pointed_at_the_users_repository() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, managed_path) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let target_before = fingerprint(&target);

    // Something we can recognise later, invisible to the emptiness predicate
    // because it lives inside `.git/`.
    std::fs::write(managed_path.join(".git").join("fixture-witness"), b"old\n").unwrap();
    assert!(trash_entries(&b.workspace_root).is_empty());

    let (status, body) = repoint_to(&b, &track, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (kind, path, frozen) = workspace_row(&b, &track).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(&path), target);
    assert!(
        frozen.is_some(),
        "`attached -> *` is not a legal transition, so this is a one-way door and \
         the row must say so; S4's `no_attached_track_is_ever_unfrozen` pins the \
         same thing over the whole table"
    );

    assert_eq!(
        diff(&target_before, &fingerprint(&target)),
        Vec::<String>::new(),
        "the server must not have touched the user's repository"
    );

    // The OLD managed directory is in the trash — moved, not deleted.
    let trashed = trash_entries(&b.workspace_root);
    assert_eq!(trashed.len(), 1, "expected exactly one trash entry");
    assert!(
        trashed[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&track),
        "trash entry {trashed:?} should be named after the track"
    );
    assert_eq!(
        std::fs::read_to_string(trashed[0].join(".git").join("fixture-witness")).unwrap(),
        "old\n",
        "the old workspace must be MOVED, not deleted"
    );
    assert!(
        !managed_path.exists(),
        "the old managed directory must be gone from its original path"
    );

    let claims: Vec<(String, String)> = sqlx::query_as("SELECT path, area_id FROM area_folders")
        .fetch_all(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        claims,
        vec![(target.to_string_lossy().into_owned(), area.clone())],
        "`attach_folder: true` must claim the directory for the track's area"
    );
}

#[tokio::test]
async fn a_second_repoint_is_refused_because_the_first_one_froze_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    let first = user_repo(&b.tmp.path().join("first"));
    let second = user_repo(&b.tmp.path().join("second"));

    let (status, body) = repoint_to(&b, &track, &first).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (status, body) = repoint_to(&b, &track, &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        PathBuf::from(workspace_row(&b, &track).await.1),
        first,
        "the track must still point at the first repository"
    );
}

#[tokio::test]
async fn the_planner_harness_is_restarted_on_the_new_path_with_a_new_thread() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, managed_path) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let (status, body) = repoint_to(&b, &track, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // A resumed thread keeps the cwd it was minted with, so the restart must carry `force_new_thread: true`.
    let payloads = harness_start_payloads(&b).await;
    let last: Value = serde_json::from_str(payloads.last().expect("a harness start")).unwrap();
    assert_eq!(
        last["cwd"].as_str(),
        Some(target.to_string_lossy().as_ref()),
        "the restart must carry the track's NEW workspace path, not the trashed \
         one; payload={last}"
    );
    assert_ne!(
        last["cwd"].as_str(),
        Some(managed_path.to_string_lossy().as_ref()),
        "payload={last}"
    );
    assert_eq!(
        last["force_new_thread"],
        json!(true),
        "payload={last} — a re-point must mint a new thread"
    );
    assert_eq!(
        last["reset_harness_items"],
        json!(false),
        "payload={last} — harness items are persisted per card, so re-opening \
         the thread must not wipe the user's transcript"
    );
}

/// The restart's `force_new_thread: true` supersedes the old runtime on its own, so only an in-window check can see the fence.
#[tokio::test]
async fn the_fence_is_up_before_the_move_not_after_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: a freshly created track has an active planner-harness runtime, \
         otherwise this test cannot observe the fence at all"
    );

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_workspace_repoint_race_hook_for_test(
        &track,
        calm_server::routes::tracks::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let track_for_task = track.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/tracks/{track_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });

    entered.notified().await;
    // In the window: the fence transaction has committed and nothing has moved yet.
    let still_active: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    release.notify_one();
    let (status, body) = patch.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        still_active.is_empty(),
        "runtimes {still_active:?} were still active while the workspace was \
         about to be renamed into the trash. \
         `session_projection_active_for_card` is what the dispatcher consults \
         before delivering an observation, so an active row in this window means \
         a push could start a turn whose writes land in `.trash`."
    );
}

/// At the moment the fence transaction commits every durable state says the workspace is empty; only the pre-move re-check can see this write.
#[tokio::test]
async fn a_write_between_the_fence_and_the_move_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_workspace_repoint_race_hook_for_test(
        &track,
        calm_server::routes::tracks::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let track_for_task = track.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/tracks/{track_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });

    entered.notified().await;
    std::fs::write(
        path.join("agent-output.md"),
        b"the turn was still running\n",
    )
    .unwrap();
    release.notify_one();

    let (status, body) = patch.await.unwrap();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write in the fence→move window must abort the re-point; body={body}"
    );

    assert!(
        trash_entries(&b.workspace_root).is_empty(),
        "the workspace must not have been renamed into the trash"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("agent-output.md")).unwrap(),
        "the turn was still running\n",
        "the racing turn's output must still be where it was written"
    );
    assert_eq!(
        workspace_path(&b, &track).await,
        path,
        "the stored path must be unchanged after a refusal"
    );
    // The fence transaction commits, so its claim pass must be scan-only or a claim row would outlive this refusal.
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        claims, 0,
        "a refusal after the fence must leave no folder claim behind"
    );
}

#[tokio::test]
async fn a_plain_file_in_the_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    std::fs::write(path.join("notes.md"), b"the agent wrote this\n").unwrap();

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("notes.md")).unwrap(),
        "the agent wrote this\n"
    );
}

/// `.claude/worktrees/` is excluded by `.git/info/exclude`, so only the `--ignored` clause sees it.
#[tokio::test]
async fn excluded_worker_output_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&track)
        .join("c1");
    std::fs::create_dir_all(&lease).unwrap();
    std::fs::write(lease.join("report.md"), b"worker output\n").unwrap();

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "ignored worker output must block the change; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(lease.join("report.md").exists());
}

/// A commit on a slice branch leaves the working tree clean; only the
/// `rev-list --count --all` clause rejects it.
#[tokio::test]
async fn a_commit_on_a_slice_branch_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    with_identity(&path);
    git(&path, &["checkout", "-q", "-b", "neige/slice"]);
    std::fs::write(path.join("work.txt"), b"worker work\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "--no-verify", "-m", "work"]);
    git(&path, &["checkout", "-q", "main"]);

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(commit_count(&path), "2", "the commit must still be there");
}

/// The shape production actually produces; the status clause rejects it before the worktree clause does.
#[tokio::test]
async fn a_lease_worktree_at_the_real_lease_path_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&track)
        .join("c1");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/lease",
            lease.to_str().unwrap(),
        ],
    );

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(lease.join(".git").exists());
}

/// Clean by every other measure; only `git worktree list` says otherwise. Moving it would dangle the absolute pointers in both directions.
#[tokio::test]
async fn a_worktree_outside_the_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let elsewhere = b.tmp.path().join("detached-worktree");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/detached",
            elsewhere.to_str().unwrap(),
        ],
    );

    // Premise: the other two clauses are blind to this.
    let status_out = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status_out.stdout)
            .trim()
            .is_empty(),
        "premise broken: the status clause already sees this fixture"
    );
    assert_eq!(
        commit_count(&path),
        "1",
        "premise broken: a commit was added"
    );

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(elsewhere.join(".git").exists());
}

#[tokio::test]
async fn an_attached_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": area,
            "title": "attached",
            "cwd": repo_dir.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let track = serde_json::from_str::<Value>(&text).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "attached repositories belong to the user; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(
        repo_dir.join("README.md").exists(),
        "the user's repository must not be touched"
    );
    let (kind, path, _) = workspace_row(&b, &track).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

/// `AttachedFromCwd` freezes at creation, so the row is put into `attached` + unfrozen directly — a state no route reaches today.
#[tokio::test]
async fn an_unfrozen_attached_workspace_is_still_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (track, _) = managed_track(&b, &area, "w").await;
    sqlx::query(
        "UPDATE tracks SET workspace_kind='attached', workspace_path=?1, \
         workspace_frozen_at=NULL WHERE id=?2",
    )
    .bind(repo_dir.to_string_lossy().as_ref())
    .bind(&track)
    .execute(b.repo.pool())
    .await
    .unwrap();
    let before = std::fs::read_to_string(repo_dir.join("README.md")).unwrap();

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an attached workspace must be refused on `kind` alone, with no freeze \
         stamp to fall back on; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("README.md")).unwrap(),
        before,
        "the user's repository must not have been touched"
    );
    let (kind, path, _) = workspace_row(&b, &track).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

#[tokio::test]
async fn a_system_area_track_refuses_the_change() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let track = launchpad["track_id"].as_str().unwrap().to_string();
    let before = workspace_row(&b, &track).await;

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        workspace_row(&b, &track).await,
        before,
        "the launchpad's workspace row must be untouched"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn a_managed_target_is_a_documented_400_not_a_silent_no_op() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({"workspace": {"kind": "managed", "path": path.to_string_lossy()}})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a managed path is derived from the track, so `managed -> managed` would \
         always re-derive the same directory; answering it explicitly beats \
         letting a caller believe an in-place reset was a change. body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(workspace_row(&b, &track).await.0, "managed");
}

/// Without this, a nonexistent directory is a 201 and the first codex task dies with only `spawn-failed` visible.
#[tokio::test]
async fn attaching_a_path_that_does_not_exist_is_refused_on_both_routes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let missing = b.tmp.path().join("no-such-directory");

    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": area, "title": "w", "theme": theme(),
            "cwd": missing.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body.contains("does not exist"),
        "the response must say what is wrong, not `spawn-failed`: {body}"
    );
    let tracks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE title='w'")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(tracks, 0, "a refused create must leave no track row");

    let (track, _) = managed_track(&b, &area, "patched").await;
    let (status, body) = repoint_to(&b, &track, &missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("does not exist"), "{body}");
    assert_eq!(workspace_row(&b, &track).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn attaching_a_directory_that_is_not_a_git_work_tree_is_refused_on_both_routes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    // A real directory, outside any repository, with no `.git`.
    let plain = b.tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("notes.txt"), b"just a folder\n").unwrap();

    // Premise, asserted: git discovery walks UPWARD, so a stray `.git` in `$TMPDIR` would turn the 400 below into a 201.
    let discovery = Command::new("git")
        .arg("-C")
        .arg(&plain)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    assert!(
        !discovery.status.success(),
        "premise broken: {plain:?} resolves to a Git work tree at {}. Some \
         ancestor of $TMPDIR contains a `.git` — remove it; this test needs a \
         directory that is genuinely outside every repository.",
        String::from_utf8_lossy(&discovery.stdout).trim()
    );

    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": area, "title": "w", "theme": theme(),
            "cwd": plain.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body.contains("not inside a Git work tree"),
        "the response must carry the real reason: {body}"
    );

    let (track, _) = managed_track(&b, &area, "patched").await;
    let (status, body) = repoint_to(&b, &track, &plain).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("not inside a Git work tree"), "{body}");
    assert_eq!(workspace_row(&b, &track).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn attaching_a_file_rather_than_a_directory_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let file = b.tmp.path().join("a-file");
    std::fs::write(&file, b"not a directory\n").unwrap();
    let (track, _) = managed_track(&b, &area, "w").await;

    let (status, body) = repoint_to(&b, &track, &file).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("is not a directory"), "{body}");
}

/// A subdirectory is a legal cwd: the worker path derives the repository root itself.
#[tokio::test]
async fn attaching_a_subdirectory_of_a_repository_is_allowed() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let sub = repo.join("crates");
    std::fs::create_dir_all(&sub).unwrap();
    let (track, _) = managed_track(&b, &area, "w").await;

    let (status, body) = repoint_to(&b, &track, &sub).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(PathBuf::from(workspace_row(&b, &track).await.1), sub);
}

#[tokio::test]
async fn a_directory_claimed_by_another_area_is_a_structured_conflict() {
    let b = boot().await;
    let owner = create_area(&b, "owner").await;
    let other = create_area(&b, "other").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));

    // `owner` claims it first, through the create route.
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": owner, "title": "first", "theme": theme(),
            "cwd": repo.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (track, managed_path) = managed_track(&b, &other, "w").await;
    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: the track has a live planner harness"
    );

    let (status, body) = repoint_to(&b, &track, &repo).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    let conflict: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        conflict["area_id"].as_str(),
        Some(owner.as_str()),
        "the 409 must name the area that owns the directory, not just say \
         `conflict`: {body}"
    );
    assert_eq!(
        conflict["conflict_path"].as_str(),
        Some(repo.to_string_lossy().as_ref())
    );
    assert!(conflict["conflict_kind"].is_string(), "{body}");

    let (kind, path, frozen) = workspace_row(&b, &track).await;
    assert_eq!(kind, "managed");
    assert_eq!(PathBuf::from(path), managed_path);
    assert_eq!(frozen, None);
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(managed_path.join(".git").is_dir());

    // The claim rules run in the fence transaction before the supersede, so a doomed target does not cost the user their running agent.
    let active_after: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        active_after, active_before,
        "a refused target must leave the running planner harness alone"
    );
}

#[tokio::test]
async fn an_unclaimed_directory_without_attach_folder_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let (track, _) = managed_track(&b, &area, "w").await;

    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({"workspace": {"kind": "attached", "path": repo.to_string_lossy()}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(
        body.contains("attach_folder"),
        "the refusal must say how to proceed: {body}"
    );
    assert_eq!(workspace_row(&b, &track).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn a_directory_this_area_already_claims_needs_no_new_claim() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({
            "area_id": area, "title": "first", "theme": theme(),
            "cwd": repo.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (track, _) = managed_track(&b, &area, "second").await;
    let (status, body) = repoint_to(&b, &track, &repo).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        claims, 1,
        "the second track must not mint a duplicate claim"
    );
}

#[tokio::test]
async fn a_workspace_change_cannot_ride_along_with_row_edits() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    // A target that would otherwise be accepted, so the 400 can only be coming
    // from the mixing rule.
    let target = user_repo(&b.tmp.path().join("my-project"));
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({
            "title": "renamed",
            "workspace": {
                "kind": "attached",
                "path": target.to_string_lossy(),
                "attach_folder": true,
            },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE id=?1")
        .bind(&track)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(title, "w", "the row edit must not have been applied either");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// A re-point would rename the terminal's directory into `.trash/` while a `terminals` row still points at it; nothing re-anchors `terminals.cwd`.
#[tokio::test]
async fn a_terminal_card_lands_in_the_workspace_and_freezes_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &track).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/tracks/{track}/terminal-cards"),
        Some(json!({"theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let terminal_cwds: Vec<String> = sqlx::query_scalar(
        "SELECT t.cwd FROM terminals t JOIN cards c ON c.id = t.card_id WHERE c.track_id = ?1",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(!terminal_cwds.is_empty(), "a terminal row must exist");
    for cwd in &terminal_cwds {
        assert_eq!(
            PathBuf::from(cwd),
            path,
            "#1147 S6: a terminal card with no explicit cwd must open in the \
             track's workspace, not in $HOME"
        );
    }

    assert!(
        workspace_row(&b, &track).await.2.is_some(),
        "#1147 S6 freeze point 2: persisting a terminal row freezes the workspace"
    );
    let (status, body) = repoint(&b, &track).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the track now has a durable cwd consumer; the re-point must be refused; body={body}"
    );
    assert_eq!(
        workspace_row(&b, &track).await.1,
        path.to_string_lossy(),
        "the refused re-point must not have moved the row"
    );
    assert!(
        trash_entries(&b.workspace_root).is_empty(),
        "the refused re-point must not have touched the filesystem"
    );
}

/// An explicit cwd is honored; the freeze still applies because it is the row, not the path it names, that cannot be re-anchored.
#[tokio::test]
async fn an_explicit_terminal_cwd_is_kept_and_still_freezes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let elsewhere = b.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/tracks/{track}/terminal-cards"),
        Some(json!({"theme": theme(), "cwd": elsewhere.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let cwd: String = sqlx::query_scalar(
        "SELECT t.cwd FROM terminals t JOIN cards c ON c.id = t.card_id WHERE c.track_id = ?1",
    )
    .bind(&track)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(PathBuf::from(&cwd), elsewhere);
    assert_ne!(PathBuf::from(&cwd), path);
    assert!(
        workspace_row(&b, &track).await.2.is_some(),
        "the freeze is a property of persisting the row, not of which path it names"
    );
}

#[tokio::test]
async fn leaving_draft_freezes_the_workspace_and_the_change_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &track).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({"lifecycle": "planning"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        workspace_row(&b, &track).await.2.is_some(),
        "once a track is past Draft the scheduler, the forge and every worker \
         treat the path as given, so it must be frozen"
    );

    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: the track has a live planner harness"
    );

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());

    // `frozen_at` is checked in the fence transaction, before the supersede, so a track that was never going to move does not lose its agent.
    let active_after: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&track)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        active_after, active_before,
        "a frozen track's re-point must be refused without disturbing its harness"
    );
}

#[tokio::test]
async fn the_first_workspace_lease_freezes_the_workspace() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &track).await.2, None, "premise: unfrozen");

    let card: String = sqlx::query_scalar(
        "SELECT id FROM cards WHERE track_id=?1 AND role='planner' ORDER BY created_at, id LIMIT 1",
    )
    .bind(&track)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &track,
        &card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(target.join(".git").is_dir());

    // Through the dispatcher's in-transaction entry point; the codex-cards route would need a live codex app-server.
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &card,
        &track,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert!(
        workspace_row(&b, &track).await.2.is_some(),
        "a lease row stores an absolute path and its worktree is bound to this \
         repository by two absolute pointers a rename would dangle, so the first \
         lease must close the latch"
    );
    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
}

#[tokio::test]
async fn a_child_track_is_frozen_at_creation_and_cannot_be_repointed() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (parent, _) = managed_track(&b, &area, "parent").await;
    let (child, _) = managed_track(&b, &area, "child").await;
    // The row is put into the child state directly; the production PATCH route is what gets tested.
    sqlx::query("UPDATE tracks SET parent_track_id=?1, workspace_frozen_at=?2 WHERE id=?3")
        .bind(&parent)
        .bind(1_i64)
        .bind(&child)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, body) = repoint(&b, &child).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// Every Today-panel codex task takes a lease; a frozen launchpad would make `ensure` 500 forever.
#[tokio::test]
async fn a_workspace_lease_never_freezes_the_launchpad() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let track = launchpad["track_id"].as_str().unwrap().to_string();
    let planner_card = launchpad["planner_card_id"].as_str().unwrap().to_string();
    assert_eq!(
        workspace_row(&b, &track).await.2,
        None,
        "premise: the launchpad starts unfrozen (design D9 exception)"
    );

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &track,
        &planner_card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &planner_card,
        &track,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert_eq!(
        workspace_row(&b, &track).await.2,
        None,
        "the launchpad's path is kernel-maintained and must stay re-pointable; \
         a stamp here bricks `today_launchpad_ensure_tx` against the freeze latch"
    );

    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "the Today panel must still come up after a lease: {status} {body}"
    );

    let (status, body) = repoint(&b, &track).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

/// `maybe_issue_turn` reads no durable state, so an observation enqueued before the fence committed still becomes a turn unless the live handle is shut down.
#[tokio::test]
async fn the_fence_also_takes_the_live_harness_out_of_the_registry() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let runtime_id = install_live_harness(&b, &track).await;

    let (status, body) = repoint_to(&b, &track, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        b.harness.get(&runtime_id).is_none(),
        "the fenced runtime {runtime_id} is still live in the registry, so an \
         observation enqueued before the fence committed could still become a \
         turn in the directory that just moved to the trash"
    );
}

/// A refusal after teardown without the restart would leave the track alive but its planner agent dead.
#[tokio::test]
async fn a_refusal_after_the_fence_reopens_the_harness_on_the_old_path() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, path) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    install_live_harness(&b, &track).await;
    let starts_before = harness_start_payloads(&b).await.len();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_workspace_repoint_race_hook_for_test(
        &track,
        calm_server::routes::tracks::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = b.app.clone();
    let track_for_task = track.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/tracks/{track_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });
    entered.notified().await;
    std::fs::write(
        path.join("agent-output.md"),
        b"the turn was still running\n",
    )
    .unwrap();
    release.notify_one();

    let (status, body) = patch.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");

    let payloads = harness_start_payloads(&b).await;
    assert!(
        payloads.len() > starts_before,
        "a refusal that tore the harness down must start it again; no new \
         `planner-harness-start` was submitted"
    );
    let last: Value = serde_json::from_str(payloads.last().unwrap()).unwrap();
    assert_eq!(
        last["cwd"].as_str(),
        Some(path.to_string_lossy().as_ref()),
        "the restart must use the OLD path — nothing moved. payload={last}"
    );
    assert_eq!(
        workspace_row(&b, &track).await.0,
        "managed",
        "and the row must be untouched"
    );
}

/// The failure is injected: `PlannerHarness::shutdown` only fails on a persistence error an integration test cannot provoke.
#[tokio::test]
async fn a_failed_harness_shutdown_still_completes_the_repoint() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, _) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    install_live_harness(&b, &track).await;
    let starts_before = harness_start_payloads(&b).await.len();
    calm_server::routes::tracks::fail_workspace_repoint_shutdown_for_test(&track);

    let (status, body) = repoint_to(&b, &track, &target).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a shutdown failure must not abort a re-point whose fence already \
         committed; body={body}"
    );

    let (kind, path, _) = workspace_row(&b, &track).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), target);

    let payloads = harness_start_payloads(&b).await;
    assert!(
        payloads.len() > starts_before,
        "the planner agent must have been restarted; leaving it superseded with no \
         restart is the failure mode this test exists for"
    );
}

/// `ai:codex` is the only header value `Actor::to_actor_id` maps to a non-`User` actor; `ai:planner` would pass vacuously.
#[tokio::test]
async fn a_non_user_actor_may_not_change_a_workspace() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (track, managed_path) = managed_track(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let target_before = fingerprint(&target);

    // Called directly: over HTTP an `ai:codex` actor is refused earlier by the empty-card-id guard, so no request reaches this check.
    // A live harness tells this refusal apart from the write transaction's role gate, which also answers 403 but only after teardown.
    let runtime_id = install_live_harness(&b, &track).await;

    let track_row = b.repo.track_get(&track).await.unwrap().unwrap();
    let result = calm_server::routes::tracks::repoint_track_workspace_for_test(
        &calm_server::state::RouteState::from_ref(&b.state),
        &calm_server::state::WorkerState::from_ref(&b.state),
        &calm_server::actor::Actor("ai:codex".into()),
        &track_row,
        &calm_server::model::TrackWorkspacePatch {
            kind: calm_server::model::TrackWorkspaceKind::Attached,
            path: target.to_string_lossy().into_owned(),
            attach_folder: true,
        },
    )
    .await;
    let error = result.expect_err("a non-user actor must be refused");
    assert!(
        matches!(error, calm_server::error::CalmError::Forbidden(_)),
        "expected Forbidden, got {error:?}"
    );
    assert!(
        error.to_string().contains("user-only"),
        "the refusal must be THIS guard's, not the write transaction's role \
         gate answering the same 403 four steps later: {error}"
    );
    assert!(
        b.harness.get(&runtime_id).is_some(),
        "the refusal must land BEFORE the fence: runtime {runtime_id} was torn \
         out of the registry, which means the request got as far as superseding \
         the track's runtimes before something else refused it"
    );

    let (kind, path, frozen) = workspace_row(&b, &track).await;
    assert_eq!(kind, "managed");
    assert_eq!(PathBuf::from(path), managed_path);
    assert_eq!(frozen, None);
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        diff(&target_before, &fingerprint(&target)),
        Vec::<String>::new(),
        "the user's repository must not have been touched"
    );
}
