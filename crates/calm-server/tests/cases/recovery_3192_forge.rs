use super::*;
use workspace_runtime::execution::{ExecutionReply, ExecutionState};

#[tokio::test]
async fn recovery_3192_forge_restart_rejects_success_artifact() -> CalmResult<()> {
    let mut boot = TestBoot::new().await;
    let action = boot.temp_path("interrupted.sh");
    let started = boot.temp_path("started");
    let interrupt = boot.temp_path("interrupt");
    let launches = boot.temp_path("launches");
    let result_path = boot.temp_path("result.json");
    write_script(
        &action,
        "#!/bin/sh\nprintf launch >> \"$3\"\ntouch \"$1\"\nwhile [ ! -e \"$2\" ]; do sleep 0.01; done\nkill -9 0\n",
    );
    let idem = "recovery-3192-forge";
    let op_id = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key(idem),
            payload(
                &boot,
                idem,
                vec![
                    action.display().to_string(),
                    started.display().to_string(),
                    interrupt.display().to_string(),
                    launches.display().to_string(),
                ],
                result_path.clone(),
            ),
        )
        .await?;
    wait_for_file(&started).await;
    let op = c3_execution::c3_op(&boot, &op_id).await;
    let execution_id =
        serde_json::from_value(c3_execution::c3_record(&op)["execution_id"].clone())?;
    // Even a plausible exit artifact cannot turn interrupted execution into success.
    stage_result_files(
        &result_path,
        "0",
        r#"{"oid":"forged","headRefOid":"forged"}"#,
    );
    fs::write(&interrupt, "interrupt")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                boot._execution
                    .lifecycle
                    .engine()
                    .unwrap()
                    .output(&execution_id, None)
                    .await,
                Ok(ExecutionReply::Observation {
                    state: ExecutionState::Unknown { .. },
                    ..
                })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("helper loss must be observed before executor restart");
    boot._execution.restart().await;
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { ref last_error_class, .. } if last_error_class.as_deref() == Some("gate-infra")),
        "{:?}",
        result.outcome
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    boot.runtime.sweep_parked().await?;
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    assert_eq!(fs::read_to_string(launches)?, "launch");
    Ok(())
}
