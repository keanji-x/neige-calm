//! Exercise the actual destructive route with claimless sessions from before C4.
use super::*;
use crate::db::RepoRead;
use crate::error::CalmError;
use crate::extract::Path as RoutePath;
use crate::state::{CodexClient, CodexShellState, DaemonClient, RouteState, WorkerState};
use axum::extract::{FromRef, State};
use std::sync::Arc;

async fn fixture() -> (tempfile::TempDir, Arc<SqlxRepo>, AppState) {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = crate::event::EventBus::new();
    let roles = repo.card_role_cache().clone();
    let tracks = repo.track_area_cache().clone();
    let plugin = Arc::new(crate::plugin_host::PluginHost::new_full(
        Arc::new(crate::plugin_host::PluginRegistry::empty()),
        repo.clone(),
        Default::default(),
        root.path().join("plugins"),
        Vec::new(),
        events.clone(),
        crate::state::WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient {
            data_dir: root.path().into(),
            proc_supervisor_sock: None,
        }),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(root.path().join("workspaces"));
    (root, repo, state)
}

async fn delete(state: &AppState, row: &Owner) -> crate::error::Result<axum::http::StatusCode> {
    crate::routes::tracks::delete_track(
        State(RouteState::from_ref(state)),
        State(WorkerState::from_ref(state)),
        State(CodexShellState::from_ref(state)),
        crate::actor::Actor::server_send(),
        RoutePath(row.track.clone()),
    )
    .await
}

async fn check_delete(provider: AgentProvider, proof: Option<bool>) {
    let (root, repo, state) = fixture().await;
    let row = historical(
        &repo,
        root.path(),
        provider.clone(),
        provider == AgentProvider::OpenCode,
    )
    .await;
    let route = RouteState::from_ref(&state);
    match provider {
        AgentProvider::OpenCode => {
            if let Some(proven) = proof {
                super::super::config::boot_with_sweep(
                    repo.as_ref(),
                    &route.acp_planner,
                    |_| async move {
                        if proven {
                            Ok(())
                        } else {
                            Err(CalmError::Conflict("injected boot failure".into()))
                        }
                    },
                )
                .await
                .unwrap();
            }
        }
        AgentProvider::Claude => {
            // The exact proof retained by Claude boot when its marker sweep fails.
            // No registered claim exists, so filtering to retained claims loses this fence.
            if let Some(proven) = proof {
                route
                    .claude_planner
                    .pre_c4
                    .set(crate::provider_execution::PreC4Sweep {
                        unproven: if proven {
                            Default::default()
                        } else {
                            [row.worker.clone()].into()
                        },
                    })
                    .unwrap();
            }
        }
        _ => unreachable!(),
    }
    // Re-mint a credential after boot to expose any revocation on refusal.
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
        .bind(old_hash(&row))
        .bind(&row.worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let result = delete(&state, &row).await;
    if proof == Some(true) {
        assert_eq!(result.unwrap(), axum::http::StatusCode::NO_CONTENT);
        assert!(repo.track_get(&row.track).await.unwrap().is_none());
    } else {
        assert!(matches!(result, Err(CalmError::Conflict(_))), "{result:?}");
        assert!(repo.track_get(&row.track).await.unwrap().is_some());
        assert!(repo.card_get(&row.card).await.unwrap().is_some());
        assert_eq!(token(&repo, &row).await, Some(old_hash(&row)));
    }
}

#[tokio::test]
async fn acp_pre_c4_failed_boot_refuses_track_delete() {
    check_delete(AgentProvider::OpenCode, Some(false)).await;
}
#[tokio::test]
async fn claude_pre_c4_failed_boot_refuses_track_delete() {
    check_delete(AgentProvider::Claude, Some(false)).await;
}
#[tokio::test]
async fn acp_pre_c4_uninitialized_refuses_track_delete() {
    check_delete(AgentProvider::OpenCode, None).await;
}
#[tokio::test]
async fn claude_pre_c4_uninitialized_refuses_track_delete() {
    check_delete(AgentProvider::Claude, None).await;
}
#[tokio::test]
async fn acp_pre_c4_proven_allows_track_delete() {
    check_delete(AgentProvider::OpenCode, Some(true)).await;
}
#[tokio::test]
async fn claude_pre_c4_proven_allows_track_delete() {
    check_delete(AgentProvider::Claude, Some(true)).await;
}

#[tokio::test]
async fn claude_pre_c4_card_sweep_checks_claimless_sessions_and_scope() {
    let (root, repo, state) = fixture().await;
    let row = historical(&repo, root.path(), AgentProvider::Claude, false).await;
    let other = historical(&repo, root.path(), AgentProvider::Claude, false).await;
    let route = RouteState::from_ref(&state);
    route
        .claude_planner
        .pre_c4
        .set(crate::provider_execution::PreC4Sweep {
            unproven: [row.worker.clone()].into(),
        })
        .unwrap();
    assert!(matches!(
        crate::claude_planner::lifecycle::sweep_card(
            repo.as_ref(),
            &route.claude_planner,
            &row.card,
        )
        .await,
        Err(CalmError::Conflict(_))
    ));
    assert_eq!(token(&repo, &row).await, Some(old_hash(&row)));
    crate::claude_planner::lifecycle::sweep_card(repo.as_ref(), &route.claude_planner, &other.card)
        .await
        .unwrap();
    assert_eq!(token(&repo, &other).await, None);
    assert_eq!(token(&repo, &row).await, Some(old_hash(&row)));
}
