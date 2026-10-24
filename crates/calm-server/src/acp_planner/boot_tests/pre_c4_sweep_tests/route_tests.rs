//! Destructive entries must refuse before changing credentials, shutting down, or requesting Stop.
use super::*;
use crate::harness::{PlannerHarness, PlannerHarnessParams};
use crate::model::{TrackPatch, TrackWorkspaceKind, TrackWorkspacePatch};

fn proof(state: &AppState, row: &Owner, provider: &AgentProvider, proven: bool) {
    let route = RouteState::from_ref(state);
    let proof = crate::provider_execution::PreC4Sweep {
        unproven: if proven {
            Default::default()
        } else {
            [row.worker.clone()].into()
        },
    };
    match provider {
        AgentProvider::Claude => route.claude_planner.pre_c4.set(proof).unwrap(),
        AgentProvider::OpenCode => route.acp_planner.pre_c4.set(proof).unwrap(),
        _ => unreachable!(),
    }
}

async fn install_harness(state: &AppState, row: &Owner, provider: AgentProvider) -> PlannerHarness {
    let route = RouteState::from_ref(state);
    let codex = CodexShellState::from_ref(state);
    let backend = crate::harness::backend::factory::PlannerFactory::new(
        Arc::downgrade(&codex.shared_codex_appserver),
        &route.claude_planner,
        &route.acp_planner,
    )
    .open(
        route.thread_seals.clone(),
        &route.planner_wiring(),
        WorkerState::from_ref(state).repo.clone(),
        crate::harness::wiring::PlannerRow {
            binding: crate::harness::profile::PlannerBinding {
                profile: crate::harness::profile::HarnessProfile::Planner,
                provider,
            },
            worker_session_id: &row.worker,
            card_id: &row.card,
            track_id: &row.track,
            prior_total_tokens: 0,
        },
    )
    .await
    .unwrap();
    let (harness, _ingress) = PlannerHarness::run_unstarted_for_test(
        PlannerHarnessParams {
            track_delete_locks: Default::default(),
            backend,
            worker_session_id: row.worker.clone(),
            track_id: row.track.clone().into(),
            card_id: row.card.clone().into(),
            thread_id: Some(row.thread.clone()),
            repo: WorkerState::from_ref(state).repo.clone(),
            events: state.events.clone(),
            card_role_cache: state.card_role_cache.clone(),
            track_area_cache: state.track_area_cache.clone(),
            registry: state.harness.clone(),
            config: Default::default(),
            snapshot: crate::harness::HarnessSnapshot::initial(0, vec![])
                .try_into()
                .unwrap(),
        },
        8,
    );
    state.harness.insert(row.worker.clone(), harness.clone());
    harness
}

async fn status(repo: &SqlxRepo, row: &Owner) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(&row.worker)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn repoint(provider: AgentProvider, proven: bool) {
    let (root, repo, state) = fixture().await;
    let workspace_root = root.path().join("workspaces");
    let row = owner_with_plan(
        &repo,
        root.path(),
        provider.clone(),
        &crate::db::sqlite::TrackWorkspacePlan::ManagedUnder(workspace_root.clone()),
    )
    .await;
    let row = historical_row(&repo, row, provider == AgentProvider::OpenCode).await;
    proof(&state, &row, &provider, proven);
    let track = repo.track_get(&row.track).await.unwrap().unwrap();
    let old = std::path::PathBuf::from(&track.workspace.path);
    workspace_runtime::selection::materialize_managed_workspace(&workspace_root, &old, &row.track)
        .unwrap();
    let target = root.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    assert!(
        workspace::command::isolated_git_command()
            .arg("-C")
            .arg(&target)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    let harness = install_harness(&state, &row, provider).await;
    let before = status(&repo, &row).await;
    let result = crate::routes::tracks::update_track(
        State(RouteState::from_ref(&state)),
        State(WorkerState::from_ref(&state)),
        crate::actor::Actor::server_send(),
        RoutePath(row.track.clone()),
        crate::extract::JsonBody(TrackPatch {
            workspace: Some(TrackWorkspacePatch {
                kind: TrackWorkspaceKind::Attached,
                path: target.display().to_string(),
                attach_folder: true,
            }),
            ..Default::default()
        }),
    )
    .await;
    if proven {
        assert_eq!(result.unwrap().status(), axum::http::StatusCode::OK);
        assert_eq!(
            repo.track_get(&row.track)
                .await
                .unwrap()
                .unwrap()
                .workspace
                .path,
            target.display().to_string()
        );
    } else {
        assert!(matches!(result, Err(CalmError::Conflict(_))), "{result:?}");
        assert_eq!(status(&repo, &row).await, before);
        assert_eq!(token(&repo, &row).await, Some(old_hash(&row)));
        assert!(
            !harness.is_closing_for_test(),
            "no provider shutdown may be issued"
        );
        assert!(state.harness.get(&row.worker).is_some());
        assert!(old.exists());
        assert_eq!(
            repo.track_get(&row.track)
                .await
                .unwrap()
                .unwrap()
                .workspace
                .path,
            old.display().to_string()
        );
    }
}

#[tokio::test]
async fn claude_pre_c4_repoint_refuses_before_side_effects() {
    repoint(AgentProvider::Claude, false).await;
}
#[tokio::test]
async fn acp_pre_c4_repoint_refuses_before_side_effects() {
    repoint(AgentProvider::OpenCode, false).await;
}
#[tokio::test]
async fn claude_pre_c4_proven_repoint_succeeds() {
    repoint(AgentProvider::Claude, true).await;
}
#[tokio::test]
async fn acp_pre_c4_proven_repoint_succeeds() {
    repoint(AgentProvider::OpenCode, true).await;
}

async fn area_delete(proven: bool) {
    let (root, repo, state) = fixture().await;
    let row = historical(&repo, root.path(), AgentProvider::OpenCode, true).await;
    proof(&state, &row, &AgentProvider::OpenCode, proven);
    let track = repo.track_get(&row.track).await.unwrap().unwrap();
    let mut intent = workspace_runtime::execution::test_support::intent(root.path().into());
    intent.metadata.track = workspace_runtime::execution::TrackId(row.track.clone());
    let execution = intent.metadata.id.as_str().to_owned();
    if !proven {
        // A generic unsettled execution exposes a Stop committed before the ACP sweep.
        let mut tx = repo.pool().begin().await.unwrap();
        calm_truth::db::sqlite::workspace_execution::insert(
            &mut tx,
            &execution,
            &row.track,
            &intent.metadata.alias.0,
            &serde_json::to_string(&intent.metadata).unwrap(),
            &serde_json::to_string(&intent.secrets).unwrap(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let result = crate::routes::areas::delete_area(
        State(RouteState::from_ref(&state)),
        State(WorkerState::from_ref(&state)),
        State(CodexShellState::from_ref(&state)),
        crate::actor::Actor::server_send(),
        crate::auth::Principal {
            user_id: LEGACY_OWNER_USER_ID.into(),
            display_name: "Owner".into(),
            role: "owner".into(),
            session_id: "preflight-test".into(),
        },
        RoutePath(track.area_id.to_string()),
    )
    .await;
    if proven {
        assert_eq!(result.unwrap(), axum::http::StatusCode::NO_CONTENT);
        assert!(
            repo.area_get(track.area_id.as_str())
                .await
                .unwrap()
                .is_none()
        );
    } else {
        assert!(matches!(result, Err(CalmError::Conflict(_))), "{result:?}");
        let execution = calm_truth::db::sqlite::workspace_execution::load(repo.pool(), &execution)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            execution.desired, "run",
            "no Stop may be persisted or handed to a driver"
        );
        assert_eq!(status(&repo, &row).await, "idle");
        assert_eq!(token(&repo, &row).await, Some(old_hash(&row)));
        assert!(repo.track_get(&row.track).await.unwrap().is_some());
        assert!(
            repo.area_get(track.area_id.as_str())
                .await
                .unwrap()
                .is_some()
        );
    }
}
#[tokio::test]
async fn acp_pre_c4_area_delete_refuses_before_stop() {
    area_delete(false).await;
}
#[tokio::test]
async fn acp_pre_c4_proven_area_delete_succeeds() {
    area_delete(true).await;
}
