//! Native production entry: private registered Pipe execution, never a probe implementation.
use super::execution_support::*;
use calm_session::control::{IoMode, execution::*};
use std::time::Duration;

async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("native production entry timed out");
}
fn request(
    h: &Harness,
    client: &RegisteredExecutorConnection,
    name: &str,
    script: &str,
) -> PrepareExecution {
    let root = h.root(name, false);
    let authority = authority();
    let (scope, registration) = external(root, &authority);
    prepare(
        client,
        scope,
        &registration,
        authority,
        name,
        IoMode::Pipe,
        script,
    )
}
async fn stop(
    client: &mut RegisteredExecutorConnection,
    request: &PrepareExecution,
    receipt: &ExecutionReceipt,
) {
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(request, receipt, ExecutionControlPurpose::Lifecycle),
                action: ProcessAction::Stop,
            }))
            .await
            .unwrap(),
        ExecutionReply::ActionComplete
    ));
}

#[tokio::test]
async fn native_macos_private_pipe_succeeds_and_nonzero_exits() {
    bounded(async {
        let harness = Harness::start().await;
        let mut client = harness.connect().await;
        for code in [0, 23] {
            let request = request(
                &harness,
                &client,
                &format!("native-{code}"),
                &format!("printf native-output; exit {code}"),
            );
            let receipt = launch(&mut client, &request).await;
            let (state, output) = wait_for(&mut client, &request, &receipt, |state, _| {
                state.permits_handoff()
            })
            .await;
            assert_eq!(
                state,
                ExecutionState::OriginalGroupEmpty { status: Some(code) }
            );
            assert!(matches!(
                receipt.process,
                NativeProcessIdentity::MacOwnedGroup(_)
            ));
            assert_eq!(output, "native-output");
        }
    })
    .await;
}

#[tokio::test]
async fn native_macos_private_root_first_cleans_group_and_preserves_control() {
    bounded(async {
        let harness = Harness::start().await;
        let mut client = harness.connect().await;
        let control = request(&harness, &client, "control", "printf ready; exec sleep 30");
        let control_receipt = launch(&mut client, &control).await;
        wait_for(&mut client, &control, &control_receipt, |_, output| {
            output == "ready"
        })
        .await;
        let payload = request(
            &harness,
            &client,
            "root-first",
            "sleep 30 & printf 'child=%s' $!; exit 17",
        );
        let receipt = launch(&mut client, &payload).await;
        let (state, output) = wait_for(&mut client, &payload, &receipt, |state, _| {
            state.permits_handoff()
        })
        .await;
        assert_eq!(
            state,
            ExecutionState::OriginalGroupEmpty { status: Some(17) }
        );
        assert!(output.starts_with("child="));
        assert_eq!(
            observe(&mut client, &control, &control_receipt).await.0,
            ExecutionState::Spawned
        );
        stop(&mut client, &control, &control_receipt).await;
        wait_for(&mut client, &control, &control_receipt, |state, _| {
            state.permits_handoff()
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn native_macos_private_term_resistant_stop_requires_kill() {
    bounded(async {
        let harness = Harness::start().await;
        let mut client = harness.connect().await;
        let payload = request(
            &harness,
            &client,
            "resistant",
            "trap '' TERM; printf ready; exec sleep 30",
        );
        let receipt = launch(&mut client, &payload).await;
        wait_for(&mut client, &payload, &receipt, |_, output| {
            output == "ready"
        })
        .await;
        stop(&mut client, &payload, &receipt).await;
        tokio::time::sleep(STOP_GRACE / 2).await;
        assert_eq!(
            observe(&mut client, &payload, &receipt).await.0,
            ExecutionState::Spawned
        );
        assert_eq!(
            wait_for(&mut client, &payload, &receipt, |state, _| state
                .permits_handoff())
            .await
            .0,
            ExecutionState::OriginalGroupEmpty { status: None }
        );
    })
    .await;
}

#[tokio::test]
async fn native_macos_private_panic_cleanup_reaps_before_supervisor_exit() {
    // Returning early exercises the same RAII path as test unwind. The supervisor
    // watchdog owns the only Child handle and joins the bounded shutdown on Drop.
    bounded(async {
        let harness = Harness::start().await;
        let mut client = harness.connect().await;
        let payload = request(
            &harness,
            &client,
            "cleanup",
            "trap '' TERM; printf ready; exec sleep 30",
        );
        let receipt = launch(&mut client, &payload).await;
        wait_for(&mut client, &payload, &receipt, |_, output| {
            output == "ready"
        })
        .await;
        drop(client);
        drop(harness);
    })
    .await;
}

#[tokio::test]
async fn native_macos_private_paths_explicit_environment_and_pty_refusal() {
    bounded(async {
        let harness = Harness::start().await;
        let mut client = harness.connect().await;
        let mut payload = request(&harness, &client, "native path ' 空格",
            "test \"$PWD\" = \"$EXPECTED_DIR\" && test -z \"${NEIGE_EXECUTION_AMBIENT_SENTINEL+x}\" && printf '%s' \"$EXPLICIT\"");
        let PreparedSource::Private(transfer) = &payload.source else { unreachable!() };
        let root = std::fs::canonicalize(&transfer.shape.root).unwrap();
        payload.specification.environment.extend([
            ("EXPECTED_DIR".into(), root.display().to_string()),
            ("EXPLICIT".into(), "explicit value ' 空格".into()),
        ]);
        let receipt = launch(&mut client, &payload).await;
        let (state, output) = wait_for(&mut client, &payload, &receipt, |s, _| s.permits_handoff()).await;
        assert_eq!(state, ExecutionState::OriginalGroupEmpty { status: Some(0) });
        assert_eq!(output, "explicit value ' 空格");
        let mut pty = request(&harness, &client, "unsupported-pty", "exit 0");
        pty.specification.io_mode = IoMode::Pty { cols: 80, rows: 24 };
        assert!(matches!(submit(&mut client, pty).await, ExecutionReply::Refused { .. }));
    }).await;
}

#[path = "native_macos/recovery.rs"]
mod recovery;
