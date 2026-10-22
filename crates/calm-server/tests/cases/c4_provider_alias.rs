//! Permanent executor alias slots survive hot records, tombstones and restarts.
use super::claude_planner_session_fixture::Rig;
use calm_server::{
    provider_execution as owner,
    test_seams::{PausePoint, install_pause_for_test},
};
use calm_truth::db::sqlite::{provider_execution as sql, workspace_execution as executions};
use provider::control::EditPreparation;
use std::time::Duration;
use workspace_runtime::execution::*;

async fn wait(mut ready: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn c4_rewind_alias_slots_survive_hot_and_retired_predecessors_and_restarts() {
    for retired in [false, true] {
        for restart in ["none", "kernel", "supervisor"] {
            // The hot case controls when Retire is driven; this test does not
            // exercise recovery ticks (the two lost-drive tests do).
            let mut rig =
                Rig::with_execution_interval("exit", "alias test", Duration::from_secs(3600)).await;
            std::fs::write(rig.bin("claude"), "#!/bin/sh\necho launched >> launches\ntouch ready\nwhile [ ! -f finish ]; do sleep 0.01; done\nexit 0\n").unwrap();
            let task = {
                let session = rig.session().clone();
                let thread = rig.thread.clone();
                tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
            };
            wait(async || rig.dir.path().join("ws/ready").exists()).await;
            let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
                .await
                .unwrap()
                .unwrap();
            let id = owner::execution_id(claim.record()).unwrap();
            let pause = PausePoint::default();
            if !retired {
                install_pause_for_test("c4-retire-committed", id.as_str(), pause.clone());
            }
            std::fs::write(rig.dir.path().join("ws/finish"), "").unwrap();
            if retired {
                assert!(matches!(
                    task.await.unwrap().unwrap(),
                    EditPreparation::Prepared { .. }
                ));
                wait(async || {
                    matches!(
                        rig.host.execution.engine().unwrap().output(&id, None).await,
                        Ok(ExecutionReply::Retired(_))
                    )
                })
                .await;
            } else {
                tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
                    .await
                    .unwrap();
                assert!(matches!(
                    rig.host
                        .execution
                        .engine()
                        .unwrap()
                        .output(&id, None)
                        .await
                        .unwrap(),
                    ExecutionReply::Observation { .. }
                ));
                assert!(
                    executions::load(rig.repo.pool(), id.as_str())
                        .await
                        .unwrap()
                        .unwrap()
                        .secrets_json
                        .is_some()
                );
                task.abort();
                let _ = task.await;
            }
            assert!(
                sql::load(rig.repo.pool(), &rig.worker_session_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            if restart == "supervisor" {
                rig.restart_executor().await;
            }
            let engine = if restart == "none" {
                rig.host.execution.engine().unwrap().clone()
            } else {
                rig.new_kernel(!retired).await
            };
            let observer = engine.clone();
            let session = rig
                .open_session_with_environment(vec![], Some(engine))
                .await;
            assert!(
                matches!(
                    session.prepare_edit_for_test(&rig.thread).await.unwrap(),
                    EditPreparation::Prepared { .. }
                ),
                "retired={retired}, restart={restart}"
            );
            if !retired {
                // The injected read failure removes the old recovery slot. No tick
                // can retire it until after this launch, even across kernel restart.
                assert!(observer.error(&id).is_some() || restart == "none");
                let row = executions::load(rig.repo.pool(), id.as_str())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    row.secrets_json.is_some(),
                    "predecessor retained through next Launch"
                );
                assert!(matches!(
                    observer.output(&id, None).await.unwrap(),
                    ExecutionReply::Observation { .. }
                ));
            }
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT execution_id, alias FROM workspace_executions ORDER BY execution_id",
            )
            .fetch_all(rig.repo.pool())
            .await
            .unwrap();
            assert_eq!(rows.len(), 2);
            assert_ne!(rows[0].0, rows[1].0);
            assert_ne!(rows[0].1, rows[1].1);
            assert_eq!(
                std::fs::read_to_string(rig.dir.path().join("ws/launches"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
    }
}
