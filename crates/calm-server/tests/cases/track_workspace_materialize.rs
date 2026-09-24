//! Managed workspace allocation and materialization through `POST /api/tracks`.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "ws-materialize".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    )
    .with_workspace_root(workspace_root.clone());
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        area_id: area.id.to_string(),
        repo: sqlx_repo,
        workspace_root,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn theme() -> Value {
    json!({"fg": [255, 255, 255], "bg": [0, 0, 0]})
}

/// Any valid path segment does: the lease target is derived from `<track_id>/<card_id>` and no card row is read.
const CARD_ID: &str = "card0000000000000000000000000001";

fn head_resolves(path: &std::path::Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Drives the real first-worker provisioning; do not weaken this back to `.git` + `HEAD` predicates.
async fn assert_workspace_is_usable_by_the_first_worker(b: &Boot, track_id: &str) {
    let (_, path, _) = workspace_row(&b.repo, track_id).await;
    let path = PathBuf::from(path);
    assert!(path.join(".git").is_dir(), "no repository at {path:?}");
    assert!(
        head_resolves(&path),
        "no init commit — `git worktree add` fails with `not a valid object \
         name: 'HEAD'` and the first codex worker never starts"
    );

    let worktree = calm_server::test_seams::provision_workspace_lease_for_test(
        b.repo.pool(),
        track_id,
        CARD_ID,
        &b.workspace_root,
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "the materialized workspace at {path:?} is not usable by the first \
             worker: taking a lease and provisioning its worktree failed with \
             {e}. A track in this state serves nothing but `spawn-failed` — \
             bug #1147."
        )
    });
    assert!(
        worktree.is_dir(),
        "provisioning reported success but there is no worktree at {worktree:?}"
    );
    let branch = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(
        branch.trim(),
        format!("neige/{track_id}/{CARD_ID}"),
        "the worker's worktree must be checked out on its own slice branch; a \
         worktree sharing the workspace's branch is a different bug"
    );
}

async fn workspace_row(repo: &SqlxRepo, track_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
    )
    .bind(track_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn title_only_create_allocates_and_materializes_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": b.area_id, "title": "research", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, track_id).await;
    assert_eq!(kind, "managed");
    assert_eq!(
        PathBuf::from(&path),
        b.workspace_root.join(&b.area_id).join(track_id),
        "D2 layout is `<root>/<area_id>/<track_id>`, ids only"
    );
    assert!(
        frozen.is_none(),
        "a managed workspace is a *default* and stays re-pointable until work \
         happens (design §2.3 / D4); freezing at create would make S3 vacuous"
    );

    let path = PathBuf::from(path);
    // Read before the bar below provisions a worktree into `.claude/worktrees/`.
    let exclude = std::fs::read_to_string(path.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l.trim() == ".claude/worktrees/"));
    assert!(!path.join(".gitignore").exists());

    assert_workspace_is_usable_by_the_first_worker(&b, track_id).await;
}

#[tokio::test]
async fn template_create_without_cwd_allocates_and_materializes_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": b.area_id,
            "title": "from template",
            "template_id": "small-change",
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        track["template_id"], "small-change",
        "the create must actually have taken the template branch, or this case \
         is a duplicate of the title-only one; body={body}"
    );
    let track_id = track["id"].as_str().unwrap();

    // Exactly one track: the loop below would otherwise be satisfiable by a second, differently-materialized row.
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, workspace_kind, workspace_path FROM tracks")
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly the requested track; {rows:?}"
    );

    let (kind, path, frozen) = workspace_row(&b.repo, track_id).await;
    assert_eq!(
        kind, "managed",
        "a template create with no `cwd` is managed"
    );
    assert_eq!(
        PathBuf::from(&path),
        b.workspace_root.join(&b.area_id).join(track_id),
        "D2 layout is `<root>/<area_id>/<track_id>`, ids only"
    );
    assert!(
        frozen.is_none(),
        "a managed workspace stays re-pointable until work happens (design \
         §2.3 / D4); a template create is not work"
    );

    assert_workspace_is_usable_by_the_first_worker(&b, track_id).await;
}

#[tokio::test]
async fn explicit_cwd_stays_attached_and_is_never_git_inited() {
    let b = boot().await;
    // An attached `cwd` must be inside a Git work tree, so point at a sub-directory of one.
    let user_repo = PathBuf::from(attached_repo_fixture(
        "workspace-materialize-users-own-repo",
    ));
    let target = user_repo.join("users-own-dir");
    std::fs::create_dir_all(&target).unwrap();
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": b.area_id,
            "title": "attached",
            "cwd": target.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, track["id"].as_str().unwrap()).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(&path), target);
    assert!(
        frozen.is_some(),
        "attached workspaces are frozen at creation (design D9): `attached → *` \
         is not a legal transition, so an unfrozen attached row is only ever \
         something a buggy PATCH could relocate — i.e. a user repository"
    );
    assert!(
        !target.join(".git").exists(),
        "the server `git init`-ed a directory the user pointed at"
    );
}

/// The injection is a plain file where a directory is needed (`ENOTDIR`), not a read-only parent: CI runs as root, for whom mode bits are advisory.
#[tokio::test]
async fn materialize_failure_fails_the_create() {
    let b = boot().await;
    std::fs::create_dir_all(&b.workspace_root).unwrap();
    std::fs::write(b.workspace_root.join(&b.area_id), "not a directory").unwrap();

    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": b.area_id, "title": "doomed", "theme": theme()}),
    )
    .await;
    assert!(
        !status.is_success(),
        "materialization failed but the route returned {status}; a 2xx here is \
         #1147 replayed one layer down. body={body}"
    );
    assert!(
        body.contains("materialize workspace"),
        "the response must carry the real error, not a generic one: {body}"
    );

    // Known state, deliberately pinned: materialization runs after the commit, so a failure leaves the track row behind; do not loosen this.
    let orphans: Vec<(String, String)> =
        sqlx::query_as("SELECT id, workspace_path FROM tracks WHERE title='doomed'")
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        orphans.len(),
        1,
        "known state: the track row survives a failed materialization"
    );
    assert!(
        !std::path::Path::new(&orphans[0].1).exists(),
        "the orphan row's managed path must not exist on disk — if it does, \
         materialization partially succeeded and this injection is not testing \
         what it claims: {orphans:?}"
    );

    std::fs::remove_file(b.workspace_root.join(&b.area_id)).unwrap();
    let (status, body) = post(
        b.app,
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": b.area_id, "title": "fine", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
}

/// Construction: `git branch -m neige` leaves `.git` and `HEAD` intact, but `refs/heads/neige` as a file blocks `git worktree add -b neige/<track>/<card>`.
#[tokio::test]
async fn a_materialized_workspace_can_pass_the_git_and_head_checks_and_still_fail_the_first_worker()
{
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": b.area_id, "title": "escape", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();
    let (_, path, _) = workspace_row(&b.repo, &track_id).await;
    let path = PathBuf::from(path);

    let renamed = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["branch", "-m", "neige"])
        .output()
        .unwrap();
    assert!(
        renamed.status.success(),
        "git branch -m neige: {}",
        String::from_utf8_lossy(&renamed.stderr)
    );

    // Half 1: the old bar sees nothing.
    assert!(
        path.join(".git").is_dir(),
        "the construction was supposed to leave the repository in place"
    );
    assert!(
        head_resolves(&path),
        "the construction was supposed to leave HEAD resolvable — if this fails \
         the case no longer demonstrates that `.git` + HEAD is a weak bar"
    );

    // Half 2: the production first-worker path fails.
    let err = calm_server::test_seams::provision_workspace_lease_for_test(
        b.repo.pool(),
        &track_id,
        CARD_ID,
        &b.workspace_root,
    )
    .await
    .expect_err(
        "a workspace whose only branch is `neige` must not provision a \
         `neige/<track>/<card>` worktree — if this now succeeds, git changed \
         its ref-namespace rules and this case's premise is stale",
    );
    let msg = err.to_string();
    // Matched on the ref names, not on git's prose: the wording of the conflict
    // is localized (`LANG`/`LC_ALL` reach the child), the two ref paths are not.
    assert!(
        msg.contains("git worktree add") && msg.contains("refs/heads/neige'"),
        "the failure must be the `refs/heads/neige` file/directory conflict, \
         not some other error that would make this case pass vacuously: {msg}"
    );
    assert!(
        msg.contains(&format!("refs/heads/neige/{track_id}/{CARD_ID}")),
        "the blocked ref must be this worker's slice branch: {msg}"
    );
}

#[tokio::test]
async fn an_unmaterialized_managed_track_heals_when_a_worker_takes_its_lease() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": b.area_id, "title": "orphan", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();
    let (_, path, _) = workspace_row(&b.repo, &track_id).await;

    // Reproduce the orphan state: the row exists, the directory does not.
    std::fs::remove_dir_all(&path).unwrap();
    assert!(!std::path::Path::new(&path).exists());

    // The production lease path a codex worker takes.
    let mut tx = b.repo.pool().begin().await.unwrap();
    let repo_root = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &track_id,
        CARD_ID,
        &b.workspace_root,
    )
    .await
    .expect(
        "taking a lease on an un-materialized managed track must repair it, not fail: \
         a permanently `spawn-failed` track is bug #1147 itself",
    );
    tx.commit().await.unwrap();

    assert_eq!(repo_root, std::fs::canonicalize(&path).unwrap());
    // `prepare_…` is idempotent, so re-running it inside the bar is the same repair a second worker would drive.
    assert_workspace_is_usable_by_the_first_worker(&b, &track_id).await;
}

/// A `main` branch with one commit: `git worktree add` on an unborn HEAD fails differently and would let these cases pass vacuously.
fn init_user_repo(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-b", "main"]);
    run_git(
        dir,
        &[
            "-c",
            "user.name=neige-test",
            "-c",
            "user.email=neige-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-m",
            "seed",
        ],
    );
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let output = calm_server::test_seams::neige_git_command_for_test()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run git {args:?} in {dir:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The assertions name the ref rather than the prose: git's wording is localized, `refs/heads/neige` is not.
#[tokio::test]
async fn attaching_a_repo_that_already_has_a_neige_branch_is_refused() {
    let b = boot().await;
    let tmp = TempDir::new().unwrap();
    let user_repo = tmp.path().join("users-own-repo");
    init_user_repo(&user_repo);
    run_git(&user_repo, &["branch", "neige"]);

    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": b.area_id,
            "title": "attached-neige",
            "cwd": user_repo.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a repository holding `refs/heads/neige` can never host a \
         `neige/<track>/<card>` slice branch, so accepting it only defers the \
         failure to the first worker (#1387); body={body}"
    );
    assert!(
        body.contains("refs/heads/neige"),
        "the refusal must name the offending ref, or the user cannot act on \
         it: {body}"
    );
    assert!(
        body.contains("neige/<track>/<card>"),
        "the refusal must say what the ref collides with: {body}"
    );
}

/// Admission is a point-in-time answer; nothing stops the user creating the branch after attaching.
#[tokio::test]
async fn a_neige_branch_created_after_attach_still_blocks_the_first_worker() {
    let b = boot().await;
    let tmp = TempDir::new().unwrap();
    let user_repo = tmp.path().join("users-own-repo");
    init_user_repo(&user_repo);

    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": b.area_id,
            "title": "attached-clean",
            "cwd": user_repo.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a repository with no `neige` branch must still be attachable — if \
         this is a 400 the admission check rejects the ordinary case; body={body}"
    );
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();

    // The construction, after admission has already answered.
    run_git(&user_repo, &["branch", "neige"]);

    let err = calm_server::test_seams::provision_workspace_lease_for_test(
        b.repo.pool(),
        &track_id,
        CARD_ID,
        &b.workspace_root,
    )
    .await
    .expect_err(
        "an attached repository whose refs include `refs/heads/neige` must not \
         provision a `neige/<track>/<card>` worktree — if this now succeeds, \
         git changed its ref-namespace rules and #1387's premise is stale",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("git worktree add") && msg.contains("refs/heads/neige'"),
        "the failure must be the `refs/heads/neige` file/directory conflict, \
         not some other error that would make this case pass vacuously: {msg}"
    );
    assert!(
        msg.contains(&format!("refs/heads/neige/{track_id}/{CARD_ID}")),
        "the blocked ref must be this worker's slice branch: {msg}"
    );
}
