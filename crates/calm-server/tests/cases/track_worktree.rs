//! #1830 S1: an attached create makes one kernel-made git worktree and branch for the track, based
//! on the upstream the way a worker lease is, and the Planner starts there; track delete removes
//! both. Real routes, real git: a bare origin and a clone of it as the user's checkout.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::{
    clone_for_track, configure_repo_identity, git_ref_exists, git_stdout, init_bare_origin, run_git,
};

struct Boot {
    app: axum::Router,
    state: AppState,
    area_id: String,
    repo: Arc<SqlxRepo>,
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area_id = repo
        .area_create(NewArea {
            name: "track-worktree".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap()
        .id
        .to_string();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"))
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn, None,
    ));
    let app = routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        area_id,
        repo,
        tmp,
    }
}

impl Boot {
    /// `POST /api/tracks` on the attached branch (`cwd` given).
    async fn create_at(
        &self,
        cwd: &Path,
        idempotency_key: Option<&str>,
        first_message: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "planner_provider": "codex",
            "area_id": self.area_id,
            "title": "",
            "cwd": cwd.to_string_lossy(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("content-type", "application/json");
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        self.send(builder.body(Body::from(body.to_string())).unwrap())
            .await
    }

    async fn delete_track(&self, track_id: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{track_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// A JSON request as the human actor.
    async fn as_user(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// `(workspace_path, workspace_worktree_path)` of the track row.
    async fn workspace_row(&self, track_id: &str) -> (String, Option<String>) {
        sqlx::query_as("SELECT workspace_path, workspace_worktree_path FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn only_track_id(&self) -> String {
        sqlx::query_scalar("SELECT id FROM tracks")
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// The `cwd` of every `planner-harness-start` payload of the track, oldest first.
    async fn planner_start_cwds(&self, track_id: &str) -> Vec<String> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start' \
             ORDER BY created_at_ms, id",
        )
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| serde_json::from_str::<PlannerHarnessStartOperationPayload>(&row).unwrap())
            .filter(|payload| payload.track_id == track_id)
            .map(|payload| payload.cwd)
            .collect()
    }

    async fn shutdown_harnesses(&self) {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
            .fetch_all(self.repo.pool())
            .await
            .unwrap();
        for id in ids {
            if let Some(handle) = self.state.harness.remove(&id) {
                let _ = handle.shutdown().await;
            }
        }
    }
}

/// A bare origin and a clone of it (the user's checkout, canonical), plus the seed that pushes to
/// the origin.
struct Upstream {
    origin: PathBuf,
    seed: PathBuf,
    clone: PathBuf,
}

fn upstream(root: &Path) -> Upstream {
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    init_bare_origin(&origin, &seed);
    let clone = root.join("checkout");
    clone_for_track(&origin, &clone);
    Upstream {
        origin,
        seed,
        clone: clone.canonicalize().unwrap(),
    }
}

impl Upstream {
    /// One more commit on the origin's `main`, which the clone has not fetched.
    fn advance_origin(&self, name: &str) -> String {
        std::fs::write(self.seed.join(name), "upstream\n").unwrap();
        run_git(&self.seed, ["add", "-A"]);
        run_git(&self.seed, ["commit", "-q", "-m", "upstream work"]);
        run_git(&self.seed, ["push", "-q", "origin", "main"]);
        git_stdout(&self.origin, ["rev-parse", "refs/heads/main"])
    }
}

fn head(repo: &Path) -> String {
    git_stdout(repo, ["rev-parse", "HEAD"])
}

fn porcelain(repo: &Path) -> String {
    git_stdout(repo, ["status", "--porcelain"])
}

fn expected_worktree(clone: &Path, track_id: &str) -> PathBuf {
    clone
        .join(".claude")
        .join("worktrees")
        .join(format!("track-{track_id}"))
}

/// T1: the worktree exists at the derived path, on the track branch at the origin's tip (the
/// clone is one commit behind), the clone is untouched, and the Planner starts in the worktree.
#[tokio::test]
async fn attached_create_makes_the_track_worktree_at_the_upstream() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let origin_tip = up.advance_origin("ahead.txt");
    let clone_head = head(&up.clone);
    assert_ne!(clone_head, origin_tip, "premise: the clone is behind");
    let clone_status = porcelain(&up.clone);

    let (status, body) = b.create_at(&up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let worktree = expected_worktree(&up.clone, &track_id);
    let (path, stored) = b.workspace_row(&track_id).await;
    assert_eq!(
        PathBuf::from(path),
        up.clone,
        "workspace_path stays the checkout"
    );
    assert_eq!(stored.map(PathBuf::from), Some(worktree.clone()));
    assert_eq!(
        body["workspace"]["worktree"],
        json!(worktree.to_string_lossy()),
        "{body}"
    );

    assert!(worktree.is_dir(), "the worktree exists");
    assert_eq!(head(&worktree), origin_tip, "based on the upstream tip");
    assert_eq!(
        git_stdout(&worktree, ["symbolic-ref", "HEAD"]),
        format!("refs/heads/neige/track-{track_id}")
    );
    assert_eq!(
        git_stdout(&worktree, ["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/main",
        "#2112: the track branch keeps the checkout's upstream"
    );
    assert_eq!(head(&up.clone), clone_head, "the clone's HEAD is unchanged");
    assert_eq!(porcelain(&up.clone), clone_status, "and so is its status");

    assert_eq!(
        b.planner_start_cwds(&track_id).await,
        vec![worktree.to_string_lossy().into_owned()],
        "the Planner starts in the track worktree"
    );

    // A file the Planner writes exists only in the worktree; the track's file reads resolve there.
    std::fs::write(worktree.join("planner-note.md"), "from the planner\n").unwrap();
    let (status, body) = b
        .send(
            Request::builder()
                .uri(format!(
                    "/api/tracks/{track_id}/workspace/readfile?path=planner-note.md"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["text"], "from the planner\n");
    b.shutdown_harnesses().await;
}

/// T1b: the first-message create starts its Planner in the worktree too.
#[tokio::test]
async fn a_first_message_create_starts_the_planner_in_the_track_worktree() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let origin_tip = up.advance_origin("ahead.txt");

    let (status, body) = b
        .create_at(&up.clone, Some("idem-t1b"), Some("start here"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let worktree = expected_worktree(&up.clone, &track_id);
    assert!(worktree.is_dir(), "the worktree exists");
    assert_eq!(head(&worktree), origin_tip, "based on the upstream tip");
    assert_eq!(
        b.planner_start_cwds(&track_id).await,
        vec![worktree.to_string_lossy().into_owned()]
    );
    b.shutdown_harnesses().await;
}

/// A Codex conversation's `config/read` must name the cwd its `thread/start` was given, which is
/// the track worktree (`installation_cwd`). The read happens only for a card that once chose a
/// model and then chose the default again, so the test makes that selection first.
#[tokio::test]
async fn the_codex_config_read_names_the_track_worktree() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let (status, body) = b.create_at(&up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let worktree = expected_worktree(&up.clone, &track_id);
    let card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    for selection in [
        json!({"model": "gpt-5", "reasoning_effort": null}),
        json!({"model": null, "reasoning_effort": null}),
    ] {
        let (status, body) = b
            .as_user(
                "PUT",
                &format!("/api/cards/{card_id}/planner/model"),
                selection,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }
    b.state
        .shared_codex_appserver
        .set_config_read_for_test(Default::default());
    let (status, body) = b
        .send(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/input"))
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .header("idempotency-key", calm_server::model::new_id())
                .body(Body::from(json!({"text": "which model?"}).to_string()))
                .unwrap(),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let cwds = loop {
        let cwds = b.state.shared_codex_appserver.config_read_cwds_for_test();
        if !cwds.is_empty() {
            break cwds;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the turn never read codex's config"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_eq!(cwds[0], Some(worktree.to_string_lossy().into_owned()));
    b.shutdown_harnesses().await;
}

/// A genuine retry (`#N`) of a first-message create whose harness start failed: its ensure finds
/// the directory the failed attempt made already registered, and the retry starts there.
#[tokio::test]
async fn a_genuine_retry_reuses_the_registered_track_worktree() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed, body) = b
        .create_at(&up.clone, Some("idem-retry"), Some("again"))
        .await;
    assert!(!failed.is_success(), "premise: {failed} {body}");
    let track_id = b.only_track_id().await;
    let worktree = expected_worktree(&up.clone, &track_id);
    assert!(worktree.is_dir(), "premise: the failed attempt made it");
    std::fs::write(worktree.join("planner-notes.txt"), "kept\n").unwrap();
    let branch_tip = head(&worktree);

    let (retry, body) = b
        .create_at(&up.clone, Some("idem-retry"), Some("again"))
        .await;
    assert_eq!(retry, StatusCode::CREATED, "{body}");
    assert_eq!(body["id"], json!(track_id));
    assert_eq!(
        std::fs::read_to_string(worktree.join("planner-notes.txt")).unwrap(),
        "kept\n",
        "the registered worktree is reused, not remade"
    );
    assert_eq!(head(&worktree), branch_tip);
    let cwds = b.planner_start_cwds(&track_id).await;
    assert_eq!(cwds.len(), 2, "{cwds:?}");
    assert_eq!(PathBuf::from(&cwds[1]), worktree);
    b.shutdown_harnesses().await;
}

/// A checkout with unpushed commits whose upstream moved on is refused like a worker lease
/// (`attached-repo-diverged`); the row stays, no worktree or branch is made. Once the user has
/// reconciled the checkout, a retry under the same key makes it (the documented recovery).
#[tokio::test]
async fn a_diverged_checkout_fails_the_create_and_makes_no_worktree() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let origin_tip = up.advance_origin("upstream.txt");
    std::fs::write(up.clone.join("mine.txt"), "mine\n").unwrap();
    run_git(&up.clone, ["add", "-A"]);
    run_git(&up.clone, ["commit", "-q", "-m", "unpushed work"]);

    let (status, body) = b.create_at(&up.clone, Some("idem-diverged"), None).await;
    assert!(!status.is_success(), "{status} {body}");
    assert!(
        body.to_string().contains("attached-repo-diverged"),
        "{body}"
    );
    let track_id = b.only_track_id().await;
    let worktree = expected_worktree(&up.clone, &track_id);
    assert!(!worktree.exists());
    assert!(!git_ref_exists(
        &up.clone,
        &format!("refs/heads/neige/track-{track_id}")
    ));

    run_git(&up.clone, ["fetch", "-q", "origin"]);
    run_git(&up.clone, ["rebase", "-q", "origin/main"]);
    let (status, body) = b.create_at(&up.clone, Some("idem-diverged"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["id"], json!(track_id), "the same track");
    assert!(worktree.is_dir(), "the retry made the worktree");
    assert_eq!(
        head(&worktree),
        head(&up.clone),
        "ahead of the upstream: HEAD"
    );
    assert_ne!(head(&worktree), origin_tip);
    b.shutdown_harnesses().await;
}

/// `@{upstream}` of the branch `checkout` is on, or `None`.
fn upstream_of(checkout: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// #2112: a create whose `git worktree add` fails before the branch exists (its ref directory is
/// not writable) leaves no upstream behind for the retry: the human detached the checkout
/// meanwhile, so the retried branch has none rather than the failed attempt's `origin/main`.
#[tokio::test]
async fn a_retry_after_a_failed_add_records_the_checkout_upstream_as_of_the_retry() {
    use std::os::unix::fs::PermissionsExt;
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let refs = up.clone.join(".git/refs/heads/neige");
    std::fs::create_dir_all(&refs).unwrap();
    std::fs::set_permissions(&refs, std::fs::Permissions::from_mode(0o555)).unwrap();

    let (status, body) = b.create_at(&up.clone, Some("idem-ref"), None).await;
    std::fs::set_permissions(&refs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!status.is_success(), "premise: {status} {body}");
    let track_id = b.only_track_id().await;
    let branch = format!("refs/heads/neige/track-{track_id}");
    assert!(!git_ref_exists(&up.clone, &branch), "premise: no branch");

    run_git(&up.clone, ["checkout", "-q", "--detach"]);
    let (status, body) = b.create_at(&up.clone, Some("idem-ref"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let worktree = expected_worktree(&up.clone, &track_id);
    assert_eq!(upstream_of(&worktree), None);
    b.shutdown_harnesses().await;
}

/// #2112: a create whose `git worktree add` made the branch and then failed (the worktree path
/// cannot be created) keeps the upstream recorded with that branch: the retry re-adds the existing
/// branch with it, though the human detached the checkout meanwhile.
#[tokio::test]
async fn a_retry_after_an_add_that_made_the_branch_keeps_its_upstream() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let blocker = up.clone.join(".claude/worktrees");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, "not a directory\n").unwrap();

    let (status, body) = b.create_at(&up.clone, Some("idem-path"), None).await;
    assert!(!status.is_success(), "premise: {status} {body}");
    let track_id = b.only_track_id().await;
    let branch = format!("refs/heads/neige/track-{track_id}");
    assert!(
        git_ref_exists(&up.clone, &branch),
        "premise: the branch exists"
    );

    std::fs::remove_file(&blocker).unwrap();
    run_git(&up.clone, ["checkout", "-q", "--detach"]);
    let (status, body) = b.create_at(&up.clone, Some("idem-path"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let worktree = expected_worktree(&up.clone, &track_id);
    assert_eq!(upstream_of(&worktree).as_deref(), Some("origin/main"));
    b.shutdown_harnesses().await;
}

/// A repository without a commit has nothing to check out: the create fails and leaves the row.
#[tokio::test]
async fn a_commit_less_repository_fails_the_create_and_makes_no_worktree() {
    let b = boot().await;
    let repo = b.tmp.path().join("empty");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, ["init", "-q", "-b", "main"]);
    configure_repo_identity(&repo);
    let repo = repo.canonicalize().unwrap();

    let (status, body) = b.create_at(&repo, None, None).await;
    assert!(!status.is_success(), "{status} {body}");
    let track_id = b.only_track_id().await;
    assert!(!expected_worktree(&repo, &track_id).exists());
    b.shutdown_harnesses().await;
}

/// T4: track delete discards the worktree (an untracked file too) and its branch; the clone is
/// untouched.
#[tokio::test]
async fn track_delete_removes_the_track_worktree_and_branch() {
    let b = boot().await;
    let up = upstream(b.tmp.path());
    let (status, body) = b.create_at(&up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let worktree = expected_worktree(&up.clone, &track_id);
    let branch = format!("refs/heads/neige/track-{track_id}");
    assert!(worktree.is_dir(), "premise: the worktree exists");
    std::fs::write(worktree.join("untracked.txt"), "scratch\n").unwrap();
    let clone_head = head(&up.clone);
    let clone_status = porcelain(&up.clone);
    b.shutdown_harnesses().await;

    let (status, body) = b.delete_track(&track_id).await;
    assert!(status.is_success(), "{status} {body}");
    assert!(!worktree.exists(), "the worktree directory is gone");
    let listed = git_stdout(&up.clone, ["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains(&format!("track-{track_id}")),
        "no registration survives: {listed}"
    );
    assert!(!git_ref_exists(&up.clone, &branch), "the branch is gone");
    assert_eq!(head(&up.clone), clone_head);
    assert_eq!(porcelain(&up.clone), clone_status);
}
