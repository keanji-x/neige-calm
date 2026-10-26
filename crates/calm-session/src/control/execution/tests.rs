use super::*;
use crate::{
    control::{ControlMsg, ControlReply, ProbeRequest, SignalRequest},
    read_frame, write_frame,
};
use tokio::net::UnixListener;

#[tokio::test]
async fn legacy_known_v2_probe_retains_existing_control_wire() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("v2.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(
            matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(), ControlMsg::Probe(ProbeRequest { proc_id }) if proc_id.is_empty())
        );
        write_frame(
            &mut stream,
            &ControlReply::ProbeOk {
                supervisor_version: 2,
                proc_running: false,
            },
        )
        .await
        .unwrap();
        // The released peer receives its original action, never EnsureProc or a new incarnation handshake.
        assert!(
            matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(), ControlMsg::Signal(SignalRequest { proc_id, sig: ProcSignal::Hup }) if proc_id == "existing-pty")
        );
    });
    let mut stream = connect_legacy(&path).await.unwrap();
    write_frame(
        &mut stream,
        &ControlMsg::Signal(SignalRequest {
            proc_id: "existing-pty".into(),
            sig: ProcSignal::Hup,
        }),
    )
    .await
    .unwrap();
    peer.await.unwrap();
}

#[tokio::test]
async fn legacy_unknown_versions_and_eof_never_authorize_fallback() {
    for version in [None, Some(1), Some(99)] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unknown.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(matches!(
                read_frame::<ControlMsg, _>(&mut stream).await.unwrap(),
                ControlMsg::Probe(_)
            ));
            if let Some(version) = version {
                write_frame(
                    &mut stream,
                    &ControlReply::ProbeOk {
                        supervisor_version: version,
                        proc_running: true,
                    },
                )
                .await
                .unwrap();
            }
            drop(stream);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                    .await
                    .is_err(),
                "client silently retried another control mode"
            );
        });
        assert!(connect_legacy(&path).await.is_err());
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn registered_negotiation_cannot_downgrade_to_released_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
            read_frame::<ControlMsg, _>(&mut stream).await.unwrap(),
            ControlMsg::Execution(request) if matches!(request.as_ref(), ExecutionRequest::Handshake(ExecutionHandshake {
                mode: ExecutionProtocolMode::RegisteredV3,
                ..
            }))
        ));
        write_frame(
            &mut stream,
            &ControlReply::ProbeOk {
                supervisor_version: 2,
                proc_running: true,
            },
        )
        .await
        .unwrap();
        drop(stream);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    assert!(
        RegisteredExecutorConnection::connect(&path, CapabilityToken::fresh().unwrap())
            .await
            .is_err()
    );
    peer.await.unwrap();
}

#[tokio::test]
async fn legacy_handshake_wire_stable() {
    // Released control-v3 bytes: ControlReply::Execution(14), Handshake(0),
    // envelope(0), boot string length(32), LegacyWorktree(1).
    let mut baseline = vec![14, 0, 0, 32];
    baseline.extend_from_slice(b"0123456789abcdef0123456789abcdef");
    baseline.push(1);
    let reply = ControlReply::Execution(Box::new(ExecutionReply::Handshake {
        envelope_version: 0,
        executor_boot: "0123456789abcdef0123456789abcdef"
            .to_owned()
            .try_into()
            .unwrap(),
        mode: ExecutionProtocolMode::LegacyWorktree,
    }));
    let config = bincode::config::standard();
    assert_eq!(
        bincode::serde::encode_to_vec(&reply, config).unwrap(),
        baseline
    );
    let (decoded, used): (ControlReply, _) =
        bincode::serde::decode_from_slice(&baseline, config).unwrap();
    assert_eq!(used, baseline.len());
    assert!(matches!(decoded, ControlReply::Execution(r) if matches!(*r,
        ExecutionReply::Handshake { envelope_version: 0, mode: ExecutionProtocolMode::LegacyWorktree, .. })));

    // Exercise the actual new client against an old peer speaking only frozen bytes.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("v3.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let peer = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
            read_frame::<ControlMsg, _>(&mut stream).await.unwrap(),
            ControlMsg::Probe(_)
        ));
        stream
            .write_all(b"NEIG\x00\x04\x00\x00\x00\x03\x09\x03\x00")
            .await
            .unwrap();
        let mut header = [0; 10];
        stream.read_exact(&mut header).await.unwrap();
        let len = u32::from_be_bytes(header[6..10].try_into().unwrap()) as usize;
        let mut payload = vec![0; len];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload.len(), 36);
        assert_eq!(&payload[..3], &[7, 0, 0]); // Execution, Handshake, required_contract=0
        assert_eq!(payload[35], 1); // LegacyWorktree; capability is 32 opaque bytes.
        let mut frame = b"NEIG\x00\x04".to_vec();
        frame.extend_from_slice(&(baseline.len() as u32).to_be_bytes());
        frame.extend_from_slice(&baseline);
        stream.write_all(&frame).await.unwrap();
    });
    connect_legacy(&path).await.unwrap();
    peer.await.unwrap();
}

#[test]
fn retired_cleanup_does_not_move_stop_wire_ordinal() {
    let config = bincode::config::standard();
    assert_eq!(
        bincode::serde::encode_to_vec(ProcessAction::Stop, config).unwrap(),
        [5]
    );
    assert!(matches!(
        bincode::serde::decode_from_slice::<ProcessAction, _>(&[5], config)
            .unwrap()
            .0,
        ProcessAction::Stop
    ));
    assert!(bincode::serde::decode_from_slice::<ProcessAction, _>(&[4], config).is_err());
    assert!(bincode::serde::decode_from_slice::<ProcessAction, _>(&[4, 0], config).is_err());
}

#[tokio::test]
async fn c2a_registered_negotiates_current_and_rejects_old_typed_peers() {
    for version in [8, 9, EXECUTION_ENVELOPE_VERSION] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("typed-version.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(
                matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(),
            ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Handshake(
                ExecutionHandshake { required_contract: EXECUTION_ENVELOPE_VERSION, mode: ExecutionProtocolMode::RegisteredV3, .. })))
            );
            write_frame(
                &mut stream,
                &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
                    envelope_version: version,
                    executor_boot: ExecutorBootId::fresh().unwrap(),
                    mode: ExecutionProtocolMode::RegisteredV3,
                })),
            )
            .await
            .unwrap();
        });
        let negotiated =
            RegisteredExecutorConnection::connect(&path, CapabilityToken::fresh().unwrap()).await;
        assert_eq!(negotiated.is_ok(), version == EXECUTION_ENVELOPE_VERSION);
        peer.await.unwrap();
    }
}

#[test]
fn c2a_close_stdin_appends_wire_ordinal_and_preserves_persisted_contract() {
    assert_eq!(EXECUTION_ENVELOPE_VERSION, 11);
    assert_eq!(PERSISTED_RECEIPT_CONTRACT_VERSION, 4);
    assert_eq!(
        bincode::serde::encode_to_vec(ProcessAction::CloseStdin, bincode::config::standard())
            .unwrap(),
        vec![6]
    );
}

#[test]
fn native_macos_v10_wire_roundtrips_both_identity_variants() {
    let legacy: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../calm-proc-supervisor/src/execution/fixtures/linux-v4.execution.json"
    )))
    .unwrap();
    let linux: ExecutionReceipt =
        serde_json::from_value(legacy["phase"]["Drained"]["receipt"].clone()).unwrap();
    assert!(matches!(
        linux.process,
        NativeProcessIdentity::LinuxHelper(_)
    ));
    let mut mac = linux.clone();
    mac.process = NativeProcessIdentity::MacOwnedGroup(MacProcessIdentity {
        pid: 12345,
        birth_seconds: 456,
        birth_microseconds: 789,
        nonce: linux.process.nonce().clone(),
    });
    mac.scope_mechanism = ScopeMechanismIdentity::MacOwnedGroup {
        protocol_version: 1,
    };
    for receipt in [linux, mac] {
        let bytes = bincode::serde::encode_to_vec(&receipt, bincode::config::standard()).unwrap();
        let (decoded, used): (ExecutionReceipt, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(decoded, receipt);
    }
}
