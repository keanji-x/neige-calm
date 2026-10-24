//! Observe the production boot on both sides of pass A, before its real listener.
use super::{
    claude_planner_session_fixture::{planner_card, worker_session_row},
    claude_planner_stack_fixture::{Root, Stack},
};
use calm_server::{
    execution_store::SqliteExecutionStore,
    provider_execution as owner,
    test_seams::{PausePoint, install_pause_for_test},
};
use calm_truth::db::sqlite::{
    SqlxRepo, provider_execution as sql, workspace_execution as executions,
};
use std::{sync::Arc, time::Duration};
use workspace_runtime::execution::*;

#[tokio::test]
async fn c4_boot_pass_a_precedes_listener_and_failure_is_covered_by_sweep() {
    for fail_pass_a in [false, true] {
        let root = Root::new("exit");
        let first = Stack::boot(&root).await;
        let socket = first
            .state
            .mcp_server
            .as_ref()
            .unwrap()
            .shim_config
            .socket_path
            .clone();
        let instructions = first
            .state
            .planner_wiring()
            .claude
            .instructions_dir
            .display()
            .to_string();
        first.shutdown().await;
        let repo = Arc::new(SqlxRepo::open(&root.db_url()).await.unwrap());
        let (card, _) = planner_card(&repo).await;
        let session = uuid::Uuid::new_v4().to_string();
        worker_session_row(&repo, &session, &card, &uuid::Uuid::new_v4().to_string()).await;
        let store = SqliteExecutionStore::new(repo.db().clone())
            .with_projection("claude-planner".into(), Arc::new(owner::Projection));
        let mut intent = test_support::intent(root.path().to_owned());
        intent.metadata.consumer = "claude-planner".into();
        intent.metadata.alias = ProcessAlias(format!(
            "planner:{}:{}",
            &session,
            intent.metadata.id.as_str(),
        ));
        let id = intent.metadata.id.clone();
        let mut tx = repo.pool().begin().await.unwrap();
        store.insert(&mut tx, &intent).await.unwrap();
        assert!(matches!(
            sql::admit(
                &mut tx,
                &session,
                id.as_str().into(),
                owner::Purpose::RewindCheck,
                sql::InitialRelease::Held
            )
            .await
            .unwrap(),
            sql::Cas::Applied(_)
        ));
        tx.commit().await.unwrap();
        sqlx::query("UPDATE worker_sessions SET mcp_token_hash='old-token-hash' WHERE id=?")
            .bind(&session)
            .execute(repo.pool())
            .await
            .unwrap();
        if fail_pass_a {
            sqlx::query(
                "CREATE TRIGGER c4_pass_a_failure
             BEFORE UPDATE OF provider_execution_json ON worker_sessions
             BEGIN SELECT RAISE(FAIL, 'pass A failure'); END",
            )
            .execute(repo.pool())
            .await
            .unwrap();
        }
        let revoke = PausePoint::default();
        let pass_a = PausePoint::default();
        install_pause_for_test("c4-boot-after-revoke", &instructions, revoke.clone());
        install_pause_for_test("c4-boot-after-pass-a", &instructions, pass_a.clone());
        let cfg = root.config(true);
        let boot = tokio::spawn(async move { Stack::boot_config(&cfg).await });
        tokio::time::timeout(Duration::from_secs(15), revoke.entered.notified())
            .await
            .unwrap();
        let token: Option<String> =
            sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
                .bind(&session)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert!(token.is_none());
        assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());
        revoke.release.notify_one();
        tokio::time::timeout(Duration::from_secs(15), pass_a.entered.notified())
            .await
            .unwrap();
        let claim = sql::load(repo.pool(), &session).await.unwrap().unwrap();
        let row = executions::load(repo.pool(), id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.desired, if fail_pass_a { "run" } else { "stop" });
        assert_eq!(
            claim.record().decision == owner::Decision::Observing,
            fail_pass_a
        );
        assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());
        if fail_pass_a {
            sqlx::query("DROP TRIGGER c4_pass_a_failure")
                .execute(repo.pool())
                .await
                .unwrap();
        }
        pass_a.release.notify_one();
        let stack = boot.await.unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                stack
                    .state
                    .planner_wiring()
                    .claude
                    .provider_execution
                    .sweep(repo.db(), stack.state.execution.as_deref())
                    .await
                    .unwrap();
                if sql::load(repo.pool(), &session).await.unwrap().is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            executions::load(repo.pool(), id.as_str())
                .await
                .unwrap()
                .unwrap()
                .desired,
            "retire"
        );
        stack.shutdown().await;
    }
}
