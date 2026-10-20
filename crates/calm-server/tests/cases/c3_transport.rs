//! Confirmed release survives a lost observation or idempotent stdin-close connection.
use super::c3_execution::*;
use super::*;
#[path = "../support/execution_fault.rs"]
mod fault;

async fn released_transport_error(kind: &str, close: bool) {
    let boot = TestBoot::new().await;
    let started = boot.temp_path("transport-started");
    let finish = boot.temp_path("transport-finish");
    let wait = format!(
        "touch '{}'; i=0; while [ ! -f '{}' ] && [ \"$i\" -lt 1500 ]; do i=$((i+1)); sleep .02; done; echo '{{\"oid\":\"abcdef\",\"headRefOid\":\"123456\"}}'",
        started.display(),
        finish.display()
    );
    let mut input = c3_merge_input(
        &boot,
        "transport",
        if kind == "action" { &wait } else { "exit 7" },
    );
    if kind != "action" {
        input["probe"] = json!({
            "probe_argv":["/bin/sh","-c",if kind == "verdict" { &wait } else { "true" }],
            "output_probe_argv":["/bin/sh","-c",if kind == "output" { &wait } else { "echo '{\"oid\":\"abcdef\",\"headRefOid\":\"123456\"}'" }]
        });
    }
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("transport"), input)
        .await
        .unwrap();
    wait_for_file(&started).await;
    let select = |record: &Value| {
        if kind == "action" {
            record.clone()
        } else {
            record["probes"][kind].clone()
        }
    };
    c3_wait(async || {
        select(&c3_record(&c3_op(&boot, &id).await))["release"]
            .get("Released")
            .is_some()
    })
    .await;
    let before = select(&c3_record(&c3_op(&boot, &id).await));
    let execution = before["execution_id"].as_str().unwrap();
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(execution)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    let mut fault = fault::Fault::install(&boot._execution.socket, execution.into(), close);
    tokio::time::timeout(Duration::from_secs(5), &mut fault.hit)
        .await
        .unwrap()
        .unwrap();
    // The child remains blocked, so any Stop or refusal can only come from the lost connection.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let retained = c3_op(&boot, &id).await;
    assert_eq!(retained.phase, Phase::Parked, "{kind} close={close}");
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    assert_eq!(
        select(&c3_record(&retained)),
        before,
        "{kind} close={close}"
    );
    let after: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(execution)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!(after, desired, "{kind} close={close}");
    drop(fault);
    std::fs::write(&finish, "go").unwrap();
    let result = c3_result(&boot, &id).await;
    let OperationOutcome::Succeeded { result } = result.outcome else {
        panic!("original outcome lost: {result:?}")
    };
    assert_eq!(result["event"]["head_sha"], "123456");
    assert_eq!(result["event"]["merge_sha"], "abcdef");
    assert_eq!(
        select(&c3_record(&c3_op(&boot, &id).await))["decision"],
        "Observing"
    );
    assert_eq!(forge_event_count(&boot.repo).await, 1);
    c3_all_retired(&boot, &id).await;
}

#[tokio::test]
async fn c3_released_action_output_transport_error() {
    released_transport_error("action", false).await;
}

#[tokio::test]
async fn c3_released_action_close_transport_error() {
    released_transport_error("action", true).await;
}

#[tokio::test]
async fn c3_released_verdict_output_transport_error() {
    released_transport_error("verdict", false).await;
}

#[tokio::test]
async fn c3_released_verdict_close_transport_error() {
    released_transport_error("verdict", true).await;
}

#[tokio::test]
async fn c3_released_output_output_transport_error() {
    released_transport_error("output", false).await;
}

#[tokio::test]
async fn c3_released_output_close_transport_error() {
    released_transport_error("output", true).await;
}
