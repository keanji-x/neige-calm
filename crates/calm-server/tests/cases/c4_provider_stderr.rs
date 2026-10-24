//! Real Engine Stop covers the bounded sink and its EOF/overflow evidence.
use super::claude_planner_session_fixture::RegisteredRig;
use calm_server::provider_execution::{
    self as owner,
    stderr::{self, Capture, DroppedBytes},
};
use std::{path::PathBuf, time::Duration};
use workspace_runtime::execution::*;

async fn wait(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn start(rig: &RegisteredRig, script: &str) -> (owner::Claim, owner::DriverGuard, PathBuf) {
    start_with_prefix(rig, script, "").await
}
async fn start_with_prefix(
    rig: &RegisteredRig,
    script: &str,
    prefix: &str,
) -> (owner::Claim, owner::DriverGuard, PathBuf) {
    let mut intent = test_support::intent(rig.dir.path().join("ws"));
    let id = &intent.metadata.id;
    let dir = stderr::create(&rig.host.instructions_dir, id.as_str()).unwrap();
    intent.metadata.consumer = "claude-planner".into();
    intent.metadata.alias = ProcessAlias(format!(
        "planner:{}:{}",
        &rig.worker_session_id,
        id.as_str(),
    ));
    intent
        .metadata
        .program
        .environment
        .push(("NEIGE_TEST_STDERR_DIR".into(), dir.display().to_string()));
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec![
        "-c".into(),
        stderr::wrapper(prefix),
        "sink-test".into(),
        dir.display().to_string(),
        rig.dir.path().join("ws").display().to_string(),
        "/bin/sh".into(),
        "-c".into(),
        script.into(),
    ];
    let (claim, guard) = rig
        .host
        .provider_execution
        .admit(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            calm_server::provider_execution::PreparedIntent(intent),
            owner::Purpose::RewindCheck,
        )
        .await
        .unwrap();
    (claim, guard, dir)
}
async fn stop(rig: &RegisteredRig, claim: &owner::Claim) {
    let id = owner::execution_id(claim.record()).unwrap();
    rig.host
        .provider_execution
        .stop(
            rig.repo.db(),
            rig.host.execution.engine(),
            &rig.worker_session_id,
            claim,
            owner::StopReason::Finished,
        )
        .await
        .unwrap();
    rig.host
        .execution
        .engine()
        .unwrap()
        .wait_settled(&id)
        .await
        .unwrap();
}
#[tokio::test]
async fn c4_stderr_sink_is_bounded_and_reports_overflow() {
    let rig = RegisteredRig::new("exit").await;
    let (claim, _guard, dir) = start(
        &rig,
        "head -c 1048576 /dev/zero >&2; printf 'protocol'; exit 0",
    )
    .await;
    wait(|| dir.join("stderr.closed").exists()).await;
    stop(&rig, &claim).await;
    let diagnostic = stderr::diagnostic(&dir);
    assert_eq!(diagnostic.bytes.len(), stderr::CAP);
    assert_eq!(
        diagnostic.capture,
        Capture::StderrTruncated {
            dropped_bytes: DroppedBytes::Exact(1048576 - stderr::CAP as u64)
        }
    );
    let reply = rig
        .host
        .execution
        .engine()
        .unwrap()
        .output(&owner::execution_id(claim.record()).unwrap(), Some(0))
        .await
        .unwrap();
    assert!(matches!(reply, ExecutionReply::Observation { output, .. } if output == b"protocol"));
}
#[tokio::test]
async fn c4_stderr_exact_cap_is_not_overflow() {
    let rig = RegisteredRig::new("exit").await;
    let (claim, _guard, dir) = start(&rig, "head -c 262144 /dev/zero >&2; exit 0").await;
    wait(|| dir.join("stderr.closed").exists()).await;
    stop(&rig, &claim).await;
    let diagnostic = stderr::diagnostic(&dir);
    assert_eq!(diagnostic.bytes.len(), stderr::CAP);
    assert_eq!(diagnostic.capture, Capture::Complete);
}
async fn interrupted(script: &str) {
    use tokio::io::AsyncReadExt;

    let rig = RegisteredRig::new("exit").await;
    // Save the sink job before starting the injector. File creation alone does
    // not prove head has started: let the provider fill CAP before killing the
    // EOF publisher. The injector must not retain stdin or the stderr FIFO.
    let prefix = format!(
        r#"sink_pid=$!
(
    exec 0<&- 2>&-
    until [ -f "$NEIGE_TEST_STDERR_DIR/stderr" ] &&
        [ "$(wc -c < "$NEIGE_TEST_STDERR_DIR/stderr")" -eq {cap} ]; do :; done
    kill -KILL "$sink_pid" && printf 'sink-killed'
) &
"#,
        cap = stderr::CAP
    );
    let (claim, _guard, dir) = start_with_prefix(&rig, script, &prefix).await;
    let id = owner::execution_id(claim.record()).unwrap();
    let mut reader =
        owner::stdio::EngineStdio::new(rig.host.execution.engine().unwrap().clone(), id);
    let mut confirmation = [0; 11];
    tokio::time::timeout(
        Duration::from_secs(10),
        reader.read_exact(&mut confirmation),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&confirmation, b"sink-killed");
    assert_eq!(
        std::fs::metadata(dir.join("stderr")).unwrap().len(),
        stderr::CAP as u64
    );
    assert!(!dir.join("stderr.closed").exists());
    stop(&rig, &claim).await;
    assert!(!dir.join("stderr.closed").exists());
    let diagnostic = stderr::diagnostic(&dir);
    assert_eq!(diagnostic.bytes.len(), stderr::CAP);
    assert!(
        matches!(
            diagnostic.capture,
            Capture::StderrTruncated {
                dropped_bytes: DroppedBytes::AtLeast(1) | DroppedBytes::Exact(1..)
            } | Capture::StderrCaptureInterrupted
        ),
        "{:?}",
        diagnostic.capture
    );
}
#[tokio::test]
async fn c4_stderr_overflow_survives_stop() {
    {
        let rig = RegisteredRig::new("exit").await;
        let (claim, _guard, dir) = start(
            &rig,
            "head -c 1048576 /dev/zero >&2; exec 2>/dev/null; exec sleep 120",
        )
        .await;
        wait(|| dir.join("stderr.closed").exists()).await;
        stop(&rig, &claim).await;
        assert!(matches!(
            stderr::diagnostic(&dir).capture,
            Capture::StderrTruncated { .. }
        ));
    }
    interrupted("head -c 1048576 /dev/zero >&2; exec sleep 120").await;
}

#[tokio::test]
async fn c4_stderr_stop_at_cap_is_never_untruncated() {
    interrupted("head -c 262145 /dev/zero >&2; exec sleep 120").await;
}
#[tokio::test]
async fn c4_stderr_overflow_survives_descendant_holding_fifo() {
    interrupted(
        r#"setsid sh -c 'touch descendant-ready; exec sleep 120' &
until [ -f descendant-ready ]; do sleep 0.001; done
head -c 1048576 /dev/zero >&2
wait"#,
    )
    .await;
}
#[tokio::test]
async fn c4_stderr_unreadable_never_blocks_complete() {
    let rig = RegisteredRig::new("exit").await;
    let (claim, _guard, dir) = start(&rig, "exit 0").await;
    wait(|| dir.join("stderr.closed").exists()).await;
    stop(&rig, &claim).await;
    std::fs::remove_file(dir.join("stderr")).unwrap();
    assert_eq!(stderr::diagnostic(&dir).capture, Capture::StderrUnreadable);
    rig.host
        .provider_execution
        .finish(
            rig.repo.db(),
            rig.host.execution.engine(),
            &rig.worker_session_id,
            &claim,
            None,
        )
        .await
        .unwrap();
    assert!(!dir.exists());
}
#[tokio::test]
async fn c4_stderr_sink_cleared_after_driver_loss() {
    let rig = RegisteredRig::new("exit").await;
    let (claim, guard, dir) = start(&rig, "exec sleep 120").await;
    wait(|| dir.join("stderr").exists()).await;
    drop(guard);
    rig.host
        .provider_execution
        .sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    let id = owner::execution_id(claim.record()).unwrap();
    rig.host
        .execution
        .engine()
        .unwrap()
        .wait_settled(&id)
        .await
        .unwrap();
    rig.host
        .provider_execution
        .sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    assert!(!dir.exists());
}

#[tokio::test]
async fn c4_output_gap_fails_the_read_side() {
    use tokio::io::AsyncReadExt;
    let rig = RegisteredRig::new("exit").await;
    let (claim, _guard, dir) = start(&rig, "head -c 5242880 /dev/zero; exit 0").await;
    wait(|| dir.join("stderr.closed").exists()).await;
    stop(&rig, &claim).await;
    let id = owner::execution_id(claim.record()).unwrap();
    let mut reader =
        owner::stdio::EngineStdio::new(rig.host.execution.engine().unwrap().clone(), id);
    let error = reader.read_to_end(&mut Vec::new()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
