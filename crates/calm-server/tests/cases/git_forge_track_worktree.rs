//! #1830 T5: a Planner's `git.commit` forge action runs in its track worktree, so the commit
//! lands on `neige/track-<id>` and the user's checkout is untouched. The track is minted by the
//! real create route over the fixture's repository and caches; the call goes through the real MCP
//! socket to the real git-forge plugin.

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::support::git_helpers::{clone_for_track, git_stdout, init_bare_origin};

/// `POST /api/tracks` on the attached branch, through the production router over the fixture's
/// repository, plugin host and caches (the MCP server resolves callers through the same caches).
async fn create_attached_track(fx: &Fixture, cwd: &Path) -> String {
    let repo: Arc<dyn Repo> = fx.repo.clone();
    let state = AppState::from_parts(
        repo.clone(),
        fx.events.clone(),
        Arc::new(DaemonClient {
            data_dir: fx._tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        fx.plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(fx.card_role_cache.clone()),
        Some(fx.track_area_cache.clone()),
    )
    .with_workspace_root(fx._tmp.path().join("workspaces"))
    // Down: the message-less create still answers 201, and no Planner harness outlives the test.
    .with_shared_codex_appserver(SharedCodexAppServer::new_stub_with_pending(repo, None));
    let app = calm_server::routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    let body = json!({
        "planner_provider": "codex",
        "area_id": fx.area_id,
        "title": "planner commit",
        "cwd": cwd.to_string_lossy(),
        "attach_folder": true,
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
            .unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().expect("track id").to_string()
}

/// The track's own Planner card (minted by the create route), given an MCP token and a bound
/// thread: `(raw_token, thread_id)`.
async fn planner_caller(fx: &Fixture, track_id: &str) -> (String, String) {
    let card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(track_id)
            .fetch_one(fx.repo.pool())
            .await
            .expect("the track's planner card");
    let thread_id = format!("thread-{card_id}");
    let worker_session_id = seed_runtime_thread(&fx.repo, &card_id, &thread_id).await;
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    let mut tx = fx.repo.pool().begin().await.expect("begin token tx");
    card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
        .await
        .expect("mint card MCP token");
    session_mcp_token_set_tx(&mut tx, &worker_session_id, &token_hash)
        .await
        .expect("mint session MCP token");
    tx.commit().await.expect("commit token tx");
    (token.into_inner(), thread_id)
}

#[tokio::test]
async fn planner_git_commit_lands_on_the_track_branch() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = short_tempdir("gft").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let fx = boot_fixture().await;

    let origin = fx._tmp.path().join("origin.git");
    init_bare_origin(&origin, &fx._tmp.path().join("seed"));
    let checkout = fx._tmp.path().join("checkout");
    clone_for_track(&origin, &checkout);
    let checkout = checkout.canonicalize().expect("canonical checkout");
    let track_id = create_attached_track(&fx, &checkout).await;
    let worktree = checkout
        .join(".claude/worktrees")
        .join(format!("track-{track_id}"));
    let branch = format!("neige/track-{track_id}");
    assert!(worktree.is_dir(), "premise: the track worktree exists");
    let branch_before = git_stdout(&checkout, ["rev-parse", &format!("refs/heads/{branch}")]);
    let checkout_head = git_stdout(&checkout, ["rev-parse", "HEAD"]);
    let checkout_status = git_stdout(&checkout, ["status", "--porcelain"]);

    std::fs::write(worktree.join("plan.md"), "the planner's note\n").expect("write note");
    let (raw_token, thread_id) = planner_caller(&fx, &track_id).await;
    let response = call_tool_as(
        &fx,
        &raw_token,
        &thread_id,
        20,
        COMMIT_TOOL,
        json!({ "message": "planner note", "idem": "t5", "branch": branch }),
    )
    .await;
    assert!(response.get("error").is_none(), "{response:#?}");
    assert_eq!(response["result"]["isError"], false, "{response:#?}");

    let branch_after = git_stdout(&checkout, ["rev-parse", &format!("refs/heads/{branch}")]);
    assert_ne!(
        branch_after, branch_before,
        "the commit moved the track branch"
    );
    assert_eq!(
        git_stdout(&checkout, ["log", "-1", "--format=%s", &branch_after]),
        "planner note"
    );
    assert_eq!(
        git_stdout(&checkout, ["rev-parse", "HEAD"]),
        checkout_head,
        "the checkout's HEAD is unchanged"
    );
    assert_eq!(
        git_stdout(&checkout, ["status", "--porcelain"]),
        checkout_status
    );
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}
