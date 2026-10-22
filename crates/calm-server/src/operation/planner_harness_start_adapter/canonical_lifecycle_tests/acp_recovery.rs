//! ACP retained activation is a local identity, independent of native setup.
use super::*;
use calm_truth::db::RepoRead;
use serde_json::Value;

#[tokio::test]
async fn canonical_acp_retained_activation_replay_preserves_local_binding_without_spawn() {
    let mut fx = Fixture::with_provider(false, AgentProvider::OpenCode).await;
    let workspace = fx
        .repo
        .track_get(&fx.payload.track_id)
        .await
        .unwrap()
        .unwrap()
        .workspace;
    std::fs::write(fx._dir.path().join("scenario"), "reply").unwrap();
    std::fs::create_dir_all(fx._dir.path().join("data")).unwrap();
    fx.state = fx.state.with_acp_planner_config(
        crate::acp_planner::config::AcpPlannerConfig {
            agents: vec![crate::acp_planner::config::AcpAgentConfig {
                provider: AgentProvider::OpenCode,
                command: "/usr/bin/python3".into(),
                args: vec![
                    "-u".into(),
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/acp_planner/agent.py"
                    )
                    .into(),
                ],
                env: [(
                    "ACP_FIXTURE_ROOT".into(),
                    fx._dir.path().to_string_lossy().into_owned(),
                )]
                .into(),
                min_agent_version: semver::Version::new(1, 0, 0),
                default_permission_mode: None,
            }],
        },
        &fx._dir.path().join("data"),
        fx._dir.path().join("shim"),
        fx._dir.path().join("mcp.sock"),
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "retained-local-bound",
        fx.card.as_str(),
        pause.clone(),
    );
    let (auth, token, input) = source().await;
    let wake = wake(&fx, input);
    let observe = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
            .await
            .unwrap();
        let row = fx
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap();
        assert!(uuid::Uuid::parse_str(row.thread_id.as_deref().unwrap()).is_ok());
        assert!(row.agent_session_id.is_none());
        assert!(row.mcp_token_hash.is_none());
        assert!(!fx._dir.path().join("requests.jsonl").exists());
        auth.sessions.remove(&token).unwrap();
        pause.release.notify_one();
        row
    };
    let (result, before) = tokio::join!(wake, observe);
    assert!(result.is_err());
    let (auth, token, input) = source().await;
    // Replay binding is already committed; a revoked issuer must not cause a
    // historical fallback or allocate another local/native conversation.
    auth.sessions.remove(&token).unwrap();
    assert!(super::wake(&fx, input).await.is_err());
    let after = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.thread_id, before.thread_id);
    assert_eq!(after.spawn_op_id, before.spawn_op_id);
    assert_eq!(after.agent_session_id, before.agent_session_id);
    assert!(!fx._dir.path().join("requests.jsonl").exists());
    let reservation = fx.state.harness.try_reserve(fx.worker.clone()).unwrap();
    let (_loser_auth, _loser_token, input) = source().await;
    assert!(super::wake(&fx, input).await.is_err());
    assert!(!fx._dir.path().join("requests.jsonl").exists());
    drop(reservation);
    let (_auth, _token, input) = source().await;
    super::wake(&fx, input.clone()).await.unwrap();
    super::wake(&fx, input).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let requests =
                std::fs::read_to_string(fx._dir.path().join("requests.jsonl")).unwrap_or_default();
            if requests
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|record| record["method"] == "session/prompt")
                .count()
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, before.id);
    assert_eq!(row.track_id, before.track_id);
    assert_eq!(
        fx.repo
            .track_get(&fx.payload.track_id)
            .await
            .unwrap()
            .unwrap()
            .workspace,
        workspace
    );
    assert_eq!(row.thread_id, before.thread_id);
    assert_ne!(row.agent_session_id, row.thread_id);
    assert!(row.agent_session_id.is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operations")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        1
    );
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
}
