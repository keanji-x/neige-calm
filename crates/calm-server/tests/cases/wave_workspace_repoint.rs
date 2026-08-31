//! Issue #1147 S3 — changing a wave's workspace, driven through the real
//! `PATCH /api/waves/{id}`.
//!
//! Every test here runs the production route and then **looks at the
//! filesystem**. That discipline is inherited from S4/S5: the assertion that
//! actually stops the accident is "perform the operation for real, then go and
//! see what is on disk", not a SQL query over `workspace_path`.
//!
//! The three-step execution shape (design §更换与冻结) each has its own test:
//!
//! 1. the fence — `the_fence_is_up_before_the_move_not_after_it`. Note the
//!    name: asserting the END state proves nothing, because the harness
//!    restart supersedes the old runtime on its own. Measured.
//! 2. the pre-move re-check — `a_write_between_the_fence_and_the_move_is_refused`
//!    (the ONE timing predicate in this design; a static state assertion
//!    cannot stand in for it)
//! 3. the move's own assertions — inherited from S5's
//!    `recycle_wave_workspace`, exercised here end to end
//!
//! and the four refusals (frozen / attached / system cove / non-empty) each
//! have one too. The freeze latch's system-cove exclusion has
//! `a_workspace_lease_never_freezes_the_launchpad`; the freeze point S3 does
//! NOT implement has `a_terminal_card_does_not_freeze_the_workspace_yet_n17`.

#![cfg(unix)]

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
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness (same shape as `wave_workspace_recycle.rs`)
// ---------------------------------------------------------------------------

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    #[allow(dead_code)]
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
    sqlx_repo.seed_card_role_cache(&roles).await.unwrap();
    sqlx_repo.seed_wave_cove_cache(&waves).await.unwrap();
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
        WriteContext::new(roles.clone(), waves.clone()),
    ));
    let state = AppState::from_parts(
        repo,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        sqlx_repo.clone(),
        None,
    ))
    .with_workspace_root(workspace_root.clone());
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo: sqlx_repo,
        workspace_root,
        tmp,
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

async fn create_cove(b: &Boot, name: &str) -> String {
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/coves",
        Some(json!({"name": name, "color": "#abc"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let cove: Value = serde_json::from_str(&body).unwrap();
    cove["id"].as_str().unwrap().to_string()
}

/// A managed wave: the title-only create the new FE sends.
async fn managed_wave(b: &Boot, cove_id: &str, title: &str) -> (String, PathBuf) {
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({"cove_id": cove_id, "title": title, "theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let wave: Value = serde_json::from_str(&text).unwrap();
    let id = wave["id"].as_str().unwrap().to_string();
    let path = workspace_path(b, &id).await;
    assert!(
        path.join(".git").is_dir(),
        "expected a materialized repository at {path:?}"
    );
    (id, path)
}

async fn workspace_path(b: &Boot, wave_id: &str) -> PathBuf {
    let path: String = sqlx::query_scalar("SELECT workspace_path FROM waves WHERE id=?1")
        .bind(wave_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    PathBuf::from(path)
}

async fn workspace_row(b: &Boot, wave_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id=?1",
    )
    .bind(wave_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap()
}

/// `PATCH /api/waves/{id}` asking for a managed workspace.
async fn repoint(b: &Boot, wave_id: &str) -> (StatusCode, String) {
    request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave_id}"),
        Some(json!({"workspace": {"kind": "managed"}})),
    )
    .await
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

fn marker(path: &Path) -> Option<String> {
    std::fs::read_to_string(path.join(".git").join("neige-workspace"))
        .ok()
        .map(|s| s.trim().to_string())
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

/// A user-owned repository outside the managed root.
fn user_repo(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-b", "main"]);
    with_identity(at);
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-q", "--no-verify", "-m", "user commit"]);
    at.to_path_buf()
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_pristine_managed_workspace_is_moved_to_trash_and_replaced() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;

    // Something we can recognise later, invisible to the emptiness predicate
    // because it lives inside `.git/`.
    std::fs::write(path.join(".git").join("fixture-witness"), b"old\n").unwrap();
    assert!(trash_entries(&b.workspace_root).is_empty());

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // The OLD directory is in the trash, not deleted.
    let trashed = trash_entries(&b.workspace_root);
    assert_eq!(trashed.len(), 1, "expected exactly one trash entry");
    assert!(
        trashed[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&wave),
        "trash entry {trashed:?} should be named after the wave"
    );
    assert_eq!(
        std::fs::read_to_string(trashed[0].join(".git").join("fixture-witness")).unwrap(),
        "old\n",
        "the old workspace must be MOVED, not deleted"
    );

    // A NEW workspace is materialized at the derived path, with our marker and
    // the one baseline commit — i.e. it is a workspace a worker can lease.
    let new_path = workspace_path(&b, &wave).await;
    assert_eq!(
        new_path, path,
        "a managed re-point re-derives <root>/<cove>/<wave>; the path is server-owned"
    );
    assert!(new_path.join(".git").is_dir());
    assert_eq!(marker(&new_path).as_deref(), Some(wave.as_str()));
    assert_eq!(commit_count(&new_path), "1");
    assert!(
        !new_path.join(".git").join("fixture-witness").exists(),
        "the replacement must be a fresh repository, not the old one"
    );

    let (kind, _, frozen) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "managed");
    assert_eq!(
        frozen, None,
        "a re-point does not freeze; the workspace stays a default until a \
         durable cwd consumer appears"
    );
}

#[tokio::test]
async fn the_spec_harness_is_restarted_on_the_new_path_with_a_new_thread() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // The re-point must submit a `spec-harness-start` carrying the NEW cwd and
    // `force_new_thread: true`. A resumed thread keeps the cwd it was minted
    // with, so `force_new_thread: false` would leave the spec agent working in
    // the trashed directory while every worker uses the new one.
    let payloads: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind='spec-harness-start' \
         ORDER BY created_at_ms, id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    let last: Value = serde_json::from_str(payloads.last().expect("a harness start")).unwrap();
    assert_eq!(
        last["cwd"].as_str(),
        Some(path.to_string_lossy().as_ref()),
        "the restart must carry the wave's current workspace path; payload={last}"
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

/// The fence must be up **before** the move, not merely as a side effect of
/// the restart afterwards.
///
/// Asserting the end state proves nothing: the restart carries
/// `force_new_thread: true`, which supersedes the previous runtime on its own
/// (`session_prepare_deferred_spec_tx`), so an after-the-fact check is green
/// even with the fence deleted — measured. The question is a *temporal* one —
/// "could a push still start a turn at the moment the directory moves?" — so
/// it is asked in the window, through the same hook the re-check test uses.
#[tokio::test]
async fn the_fence_is_up_before_the_move_not_after_it() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, _) = managed_wave(&b, &cove, "w").await;

    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: a freshly created wave has an active spec-harness runtime, \
         otherwise this test cannot observe the fence at all"
    );

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::waves::install_workspace_repoint_race_hook_for_test(
        &wave,
        calm_server::routes::waves::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let wave_for_task = wave.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/waves/{wave_for_task}"),
            Some(json!({"workspace": {"kind": "managed"}})),
        )
        .await
    });

    entered.notified().await;
    // In the window: the fence transaction has committed and nothing has moved
    // yet. Every runtime that was active must already be gone from the set
    // `dispatcher::harness_runtime_id_for_spec_card` reads.
    let still_active: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
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

// ---------------------------------------------------------------------------
// Step 2 — the ONE timing predicate
// ---------------------------------------------------------------------------

/// A write that lands **after** the criteria transaction committed and
/// **before** the move must abort the re-point.
///
/// This is the test the design's "SQLite 事务不能隔离文件系统写入" sentence
/// exists for, and it cannot be replaced by a static assertion: at the moment
/// the fence transaction commits, every durable state the route can read says
/// the workspace is empty. Deleting the pre-move re-check leaves this test —
/// and only this test — red.
#[tokio::test]
async fn a_write_between_the_fence_and_the_move_is_refused() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::waves::install_workspace_repoint_race_hook_for_test(
        &wave,
        calm_server::routes::waves::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let wave_for_task = wave.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/waves/{wave_for_task}"),
            Some(json!({"workspace": {"kind": "managed"}})),
        )
        .await
    });

    // The racing writer: exactly what a turn that was already in flight when
    // the fence went up would do.
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

    // Nothing moved, and nothing was lost.
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
        workspace_path(&b, &wave).await,
        path,
        "the stored path must be unchanged after a refusal"
    );
}

// ---------------------------------------------------------------------------
// The "anything on disk" refusals — one per clause, through the real route
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_plain_file_in_the_workspace_refuses_the_change() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
    std::fs::write(path.join("notes.md"), b"the agent wrote this\n").unwrap();

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("notes.md")).unwrap(),
        "the agent wrote this\n"
    );
}

/// Worker output under `.claude/worktrees/` is EXCLUDED by
/// `.git/info/exclude`, so a plain `git status --porcelain` cannot see it.
/// Only the `--ignored` clause rejects this.
#[tokio::test]
async fn excluded_worker_output_refuses_the_change() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&wave)
        .join("c1");
    std::fs::create_dir_all(&lease).unwrap();
    std::fs::write(lease.join("report.md"), b"worker output\n").unwrap();

    let (status, body) = repoint(&b, &wave).await;
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
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
    with_identity(&path);
    git(&path, &["checkout", "-q", "-b", "neige/slice"]);
    std::fs::write(path.join("work.txt"), b"worker work\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "--no-verify", "-m", "work"]);
    git(&path, &["checkout", "-q", "main"]);

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(commit_count(&path), "2", "the commit must still be there");
}

/// A lease worktree at the path leases really use. Doubly covered — the
/// checkout inside `.claude/worktrees/` is also ignored-but-present, so the
/// status clause rejects it first — which is why the single-violation fixture
/// for the worktree clause is the test below, not this one. Kept because this
/// is the shape production actually produces.
#[tokio::test]
async fn a_lease_worktree_at_the_real_lease_path_refuses_the_change() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&wave)
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

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(lease.join(".git").exists());
}

/// A worktree whose files are **outside** the workspace: the repository here is
/// clean by every other measure, and only `git worktree list` says otherwise.
///
/// Moving this repository would dangle `<wt>/.git` and
/// `<repo>/.git/worktrees/<n>/gitdir` — two absolute pointers, in both
/// directions — so the copy in the trash would not even be a usable
/// repository. Single-violation fixture for the worktree clause: measured, the
/// clause's removal turns this test red and no other integration test.
#[tokio::test]
async fn a_worktree_outside_the_workspace_refuses_the_change() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
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

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(elsewhere.join(".git").exists());
}

// ---------------------------------------------------------------------------
// The typed refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_attached_workspace_refuses_the_change() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "cove_id": cove,
            "title": "attached",
            "cwd": repo_dir.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let wave = serde_json::from_str::<Value>(&text).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = repoint(&b, &wave).await;
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
    let (kind, path, _) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

/// The `kind` guard on its own, with the freeze latch taken out of the way.
///
/// The test above is doubly covered: `AttachedFromCwd` freezes at creation, so
/// the freeze guard rejects before the kind guard is reached — measured, and
/// deleting the kind guard turns no test red. That defence in depth is
/// correct, but it leaves the guard without a fixture, and the guard is the
/// one that decides whether the server may `rename` a directory a **user**
/// owns.
///
/// So the row is put into `attached` + unfrozen directly. That state is not
/// reachable through any route today — migration 0077's comment names it
/// exactly ("an unfrozen `attached` row is exactly the state in which a future
/// PATCH branch that forgot to check `kind` would relocate a real user
/// repository") — which is the point: this asserts the guard, not the
/// reachability. Same discipline as S5's guard-4 fixture.
#[tokio::test]
async fn an_unfrozen_attached_workspace_is_still_refused() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (wave, _) = managed_wave(&b, &cove, "w").await;
    sqlx::query(
        "UPDATE waves SET workspace_kind='attached', workspace_path=?1, \
         workspace_frozen_at=NULL WHERE id=?2",
    )
    .bind(repo_dir.to_string_lossy().as_ref())
    .bind(&wave)
    .execute(b.repo.pool())
    .await
    .unwrap();
    let before = std::fs::read_to_string(repo_dir.join("README.md")).unwrap();

    let (status, body) = repoint(&b, &wave).await;
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
    let (kind, path, _) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

/// The system cove's launchpad path is kernel-maintained and is the documented
/// exception to the freeze latch, so it must be unreachable from the PATCH
/// route — otherwise the exception becomes a hole.
#[tokio::test]
async fn a_system_cove_wave_refuses_the_change() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let wave = launchpad["wave_id"].as_str().unwrap().to_string();
    let before = workspace_row(&b, &wave).await;

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        workspace_row(&b, &wave).await,
        before,
        "the launchpad's workspace row must be untouched"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn an_attached_target_is_a_documented_400_not_a_silent_no_op() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, _) = managed_wave(&b, &cove, "w").await;
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"workspace": {"kind": "attached"}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn a_workspace_change_cannot_ride_along_with_row_edits() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, _) = managed_wave(&b, &cove, "w").await;
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"title": "renamed", "workspace": {"kind": "managed"}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let title: String = sqlx::query_scalar("SELECT title FROM waves WHERE id=?1")
        .bind(&wave)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(title, "w", "the row edit must not have been applied either");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

// ---------------------------------------------------------------------------
// The freeze latch — each freeze point through its real production route
// ---------------------------------------------------------------------------

/// KNOWN GAP (#1147 N17) — freeze point 2 ("terminal persistence") is NOT
/// implemented in S3. This test asserts the gap, so the slice that closes it
/// sees red and has to replace this test rather than quietly agreeing with it.
///
/// Two reasons, both measured, both in `terminal_create_tx`'s comment:
///
/// 1. **It is not load bearing yet.** A terminal's `cwd` comes from the request
///    body or `default_cwd()`, never from `waves.workspace_path` — asserted
///    below. Re-pointing a workspace therefore cannot invalidate a terminal
///    today. S6 is the slice that makes terminals land in the wave workspace,
///    and it is the slice in which this becomes a real hole.
/// 2. **Writing `waves` from a terminal transaction deadlocks.** The in-memory
///    database runs in shared-cache mode (per-TABLE locks): with the freeze in
///    `terminal_create_tx`,
///    `claude_card_endpoint::post_claude_restart_recreates_missing_terminal_row_and_resumes_session`
///    hangs forever in `sqlx_sqlite::statement::unlock_notify::wait`. A
///    `SELECT` on `waves` from the same transaction returns fine; the `UPDATE`
///    never does. Shipping the freeze there would have traded a hole for a
///    hang.
#[tokio::test]
async fn a_terminal_card_does_not_freeze_the_workspace_yet_n17() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, path) = managed_wave(&b, &cove, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/waves/{wave}/terminal-cards"),
        Some(json!({"theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // The premise that makes the gap harmless *today*: the terminal did not
    // capture the wave's workspace path.
    let terminal_cwds: Vec<String> = sqlx::query_scalar(
        "SELECT t.cwd FROM terminals t JOIN cards c ON c.id = t.card_id WHERE c.wave_id = ?1",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(!terminal_cwds.is_empty(), "a terminal row must exist");
    for cwd in &terminal_cwds {
        assert_ne!(
            PathBuf::from(cwd),
            path,
            "KNOWN GAP (#1147 N17) is only harmless while no terminal captures \
             the wave workspace. This one did, so the gap is now a real hole: \
             close it in the slice that made terminals land in the workspace \
             (S6) and replace this test."
        );
    }

    assert_eq!(
        workspace_row(&b, &wave).await.2,
        None,
        "KNOWN GAP (#1147 N17): a terminal card does not freeze the workspace in S3"
    );
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "KNOWN GAP (#1147 N17): the re-point is still allowed; body={body}"
    );
}

/// Freeze point 3: the wave leaves Draft.
#[tokio::test]
async fn leaving_draft_freezes_the_workspace_and_the_change_is_refused() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, _) = managed_wave(&b, &cove, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"lifecycle": "planning"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "once a wave is past Draft the scheduler, the forge and every worker \
         treat the path as given, so it must be frozen"
    );
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// Freeze point 1: the first workspace lease, taken through the production
/// lease preparation + acquisition path rather than a hand-written INSERT.
#[tokio::test]
async fn the_first_workspace_lease_freezes_the_workspace() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (wave, _) = managed_wave(&b, &cove, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let card: String = sqlx::query_scalar(
        "SELECT id FROM cards WHERE wave_id=?1 AND role='spec' ORDER BY created_at, id LIMIT 1",
    )
    .bind(&wave)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &wave,
        &card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(target.join(".git").is_dir());

    // The lease acquisition itself, through the same in-transaction entry
    // point the dispatcher uses. `POST /api/waves/{id}/codex-cards` would
    // reach it too but needs a live codex app-server.
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &card,
        &wave,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "a lease row stores an absolute path and its worktree is bound to this \
         repository by two absolute pointers a rename would dangle, so the first \
         lease must close the latch"
    );
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
}

/// Freeze point 4 is S4's: a child wave is frozen the moment it is created,
/// because a spec bootstraps a harness on it immediately and there is no
/// window in which it could safely be re-pointed. Asserted here so a future
/// change to the child adapter that un-freezes it turns THIS slice red too.
#[tokio::test]
async fn a_child_wave_is_frozen_at_creation_and_cannot_be_repointed() {
    let b = boot().await;
    let cove = create_cove(&b, "c").await;
    let (parent, _) = managed_wave(&b, &cove, "parent").await;
    let (child, _) = managed_wave(&b, &cove, "child").await;
    // The child adapter's own tests cover the creation path; here the point is
    // the *state* it produces, so the row is put into that state directly and
    // the production PATCH route is what gets tested.
    sqlx::query("UPDATE waves SET parent_wave_id=?1, workspace_frozen_at=?2 WHERE id=?3")
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

/// The launchpad must survive its own freeze points.
///
/// Every codex task on the Today panel takes a workspace lease, which is freeze
/// point 1. If that stamped the launchpad, the very next
/// `POST /api/today/launchpad/ensure` would hit the latch in
/// `wave_workspace_write_tx` and 500 — a permanently dead Today panel, and the
/// panel is the one surface a user cannot route around.
///
/// The exclusion lives inside `wave_workspace_freeze_tx` as a SQL clause rather
/// than as an `if` at each freeze point, so this test drives a real freeze point
/// against the real launchpad and then re-runs `ensure`. Measured: removing the
/// clause turns this test red and no other.
#[tokio::test]
async fn a_workspace_lease_never_freezes_the_launchpad() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let wave = launchpad["wave_id"].as_str().unwrap().to_string();
    let spec_card = launchpad["spec_card_id"].as_str().unwrap().to_string();
    assert_eq!(
        workspace_row(&b, &wave).await.2,
        None,
        "premise: the launchpad starts unfrozen (design D9 exception)"
    );

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &wave,
        &spec_card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &spec_card,
        &wave,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert_eq!(
        workspace_row(&b, &wave).await.2,
        None,
        "the launchpad's path is kernel-maintained and must stay re-pointable; \
         a stamp here bricks `today_launchpad_ensure_tx` against the freeze latch"
    );

    // The consequence, stated as behaviour rather than as a column value.
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "the Today panel must still come up after a lease: {status} {body}"
    );

    // …and it is still not user-repointable.
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}
