//! #2516: the track add menu creates a Claude card with no `cwd`; it runs in the track's
//! workspace, as a terminal card does, never in the server's `$HOME`.

use axum::http::StatusCode;
use calm_server::db::prelude::*;
use serde_json::json;

use super::{ENV_LOCK, boot_success, post};

#[tokio::test]
async fn a_claude_card_created_without_a_cwd_runs_in_the_track_workspace() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let workspace: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id = ?1")
        .bind(&boot.track_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    assert!(
        !workspace.is_empty() && !home.is_empty() && workspace != home,
        "the fixture only proves anything while $HOME is set and differs: {workspace:?} {home:?}"
    );

    let body = json!({ "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]} });
    let (status, card) = post(boot.app.clone(), &boot.track_id, body, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let terminal_id = card["payload"]["terminal_id"].as_str().unwrap();
    let term = boot.repo.terminal_get(terminal_id).await.unwrap().unwrap();
    assert_eq!(term.cwd, workspace);
    assert_eq!(card["payload"]["cwd"], workspace.as_str(), "{card}");
}
