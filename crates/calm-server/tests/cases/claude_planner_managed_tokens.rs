//! Real Claude launches through the managed credential store.
use super::claude_planner_session_fixture::{
    RegisteredRig as Rig, client_id, request, until_completed,
};
use calm_server::error::CalmError;
use calm_server::harness::backend::StartDiagnostic;
use provider::TurnModelSelection;
use provider::control::TurnStartOutcome;

fn started(outcome: TurnStartOutcome<CalmError, StartDiagnostic>) -> String {
    match outcome {
        TurnStartOutcome::Started { turn } => turn.into_string(),
        other => panic!("expected Started, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_token_live_claude_rechecks_nonnull_db_hash() {
    let rig = Rig::new("exit").await;
    let mut previous = None;
    for rotate in [false, true] {
        if rotate {
            rig.repo
                .db()
                .writer()
                .execute(
                    sqlx::query(
                        "UPDATE worker_sessions SET mcp_token_hash='external-hash' WHERE id=?1",
                    )
                    .bind(&rig.worker_session_id),
                )
                .await
                .unwrap();
        }
        let mut rx = rig.session().subscribe_events();
        rig.session()
            .turn_start(
                &rig.thread,
                request(
                    rig.text("hello"),
                    &TurnModelSelection::inherit(),
                    calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                    &client_id(),
                ),
            )
            .await
            .map(started)
            .unwrap();
        until_completed(&mut rx).await;
        let env = rig.read_bin("env").unwrap();
        let token = env
            .lines()
            .find_map(|line| line.strip_prefix("NEIGE_MCP_TOKEN="))
            .unwrap()
            .to_string();
        assert_eq!(
            rig.mcp_token_hash().await,
            Some(calm_server::mcp_server::auth::hash_token(&token))
        );
        if let Some(old) = previous {
            assert_ne!(old, token);
        }
        previous = Some(token);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_token_claude_rebuilt_harness_and_host_share_valid_credential() {
    let mut rig = Rig::new("exit").await;
    let mut tokens = Vec::new();
    for rebuild in [false, true] {
        if rebuild {
            rig.rebuild_host().await;
        }
        // Each pass constructs a real installed session over the same worker row.
        let session = rig.open_session().await;
        let mut rx = session.subscribe_events();
        session
            .turn_start(
                &rig.thread,
                request(
                    rig.text("hello"),
                    &TurnModelSelection::inherit(),
                    calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                    &client_id(),
                ),
            )
            .await
            .map(started)
            .unwrap();
        until_completed(&mut rx).await;
        // TurnCompleted precedes the exact-owner claim clear; finish that real driver before
        // constructing another host/session, as registry retirement would.
        let _ = session.wait_driver_for_test().await;
        let env = rig.read_bin("env").unwrap();
        let token = env
            .lines()
            .find_map(|line| line.strip_prefix("NEIGE_MCP_TOKEN="))
            .unwrap()
            .to_string();
        assert_eq!(
            rig.mcp_token_hash().await,
            Some(calm_server::mcp_server::auth::hash_token(&token))
        );
        tokens.push(token);
    }
    assert_eq!(tokens[0], tokens[1]);
}
