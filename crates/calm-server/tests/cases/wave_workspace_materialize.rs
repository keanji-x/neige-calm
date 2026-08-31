//! Issue #1147 S2 — managed workspace allocation + materialization through
//! `POST /api/waves`.
//!
//! Design `docs/1147-workspace-design.md` D2/D3/D5 and §5 test 5. The two
//! properties this file owns:
//!
//!   * a title-only create (the #1131 shape the new FE sends) allocates a
//!     managed workspace under the configured root and leaves a real git
//!     repository there — without it, every codex task on that wave dies in
//!     `git rev-parse --show-toplevel`, which is #1147;
//!   * a materialization failure is a **non-2xx**, not a 201 with a warning in
//!     the log. The latter reproduces #1147 one layer down: the wave looks
//!     fine and the first worker dies.

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
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "ws-materialize".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
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
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    )
    .with_workspace_root(workspace_root.clone());
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        cove_id: cove.id.to_string(),
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

fn head_resolves(path: &std::path::Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn workspace_row(repo: &SqlxRepo, wave_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id=?1",
    )
    .bind(wave_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

/// Entry point 1 of 5: `POST /api/waves` with no `cwd` — the #1131 title-only
/// create the new FE sends.
#[tokio::test]
async fn title_only_create_allocates_and_materializes_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/waves",
        json!({"cove_id": b.cove_id, "title": "research", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave: Value = serde_json::from_str(&body).unwrap();
    let wave_id = wave["id"].as_str().unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, wave_id).await;
    assert_eq!(kind, "managed");
    assert_eq!(
        PathBuf::from(&path),
        b.workspace_root.join(&b.cove_id).join(wave_id),
        "D2 layout is `<root>/<cove_id>/<wave_id>`, ids only"
    );
    assert!(
        frozen.is_none(),
        "a managed workspace is a *default* and stays re-pointable until work \
         happens (design §2.3 / D4); freezing at create would make S3 vacuous"
    );

    let path = PathBuf::from(path);
    assert!(path.join(".git").is_dir(), "no repository at {path:?}");
    assert!(
        head_resolves(&path),
        "no init commit — `git worktree add` fails with `not a valid object \
         name: 'HEAD'` and the first codex worker never starts"
    );
    // D3 step 4: the exclusion lives in `.git/info/exclude`, and the fresh
    // workspace must look empty to design D4's predicate.
    let exclude = std::fs::read_to_string(path.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l.trim() == ".claude/worktrees/"));
    assert!(!path.join(".gitignore").exists());
}

/// An explicit `cwd` is the attached branch: the user pointed at that
/// directory, so the server records it and never creates or `git init`s it.
#[tokio::test]
async fn explicit_cwd_stays_attached_and_is_never_git_inited() {
    let b = boot().await;
    let target = b._tmp.path().join("users-own-dir");
    std::fs::create_dir_all(&target).unwrap();
    let (status, body) = post(
        b.app.clone(),
        "/api/waves",
        json!({
            "cove_id": b.cove_id,
            "title": "attached",
            "cwd": target.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave: Value = serde_json::from_str(&body).unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, wave["id"].as_str().unwrap()).await;
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

/// Entry point 2 of 5: `seed_workflow_template_wave`, reached by creating a
/// wave against a seeded workflow template key.
#[tokio::test]
async fn seeded_workflow_template_waves_are_materialized() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/waves",
        json!({
            "cove_id": b.cove_id,
            "title": "from template",
            "workflow_id": "small-change",
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // Every wave now on the DB — the requested one plus the three system-cove
    // templates the route seeds — must have a live repository behind it.
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, workspace_kind, workspace_path FROM waves")
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert!(
        rows.len() >= 2,
        "expected the template seed to have run; got {rows:?}"
    );
    for (id, kind, path) in &rows {
        assert_eq!(kind, "managed", "wave {id} at {path}");
        assert!(
            head_resolves(std::path::Path::new(path)),
            "template-seeded wave {id} was not materialized ({path})"
        );
    }
}

/// §5 test 5 — materialization failure must surface as a non-2xx carrying the
/// real error, not a 201 whose first worker then dies with `spawn-failed`.
///
/// The injection is a **plain file** where `<root>/<cove_id>` must be a
/// directory, so `mkdir` returns `ENOTDIR`. Deliberately not a read-only
/// parent (`chmod 0555`): CI runs as root, for whom mode bits are advisory,
/// and that injection would pass vacuously.
#[tokio::test]
async fn materialize_failure_fails_the_create() {
    let b = boot().await;
    std::fs::create_dir_all(&b.workspace_root).unwrap();
    std::fs::write(b.workspace_root.join(&b.cove_id), "not a directory").unwrap();

    let (status, body) = post(
        b.app.clone(),
        "/api/waves",
        json!({"cove_id": b.cove_id, "title": "doomed", "theme": theme()}),
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

    // And the injection really is what broke it: with the obstruction removed
    // the identical request succeeds.
    std::fs::remove_file(b.workspace_root.join(&b.cove_id)).unwrap();
    let (status, body) = post(
        b.app,
        "/api/waves",
        json!({"cove_id": b.cove_id, "title": "fine", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
}
