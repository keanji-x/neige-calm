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
async fn registered_v4_rejects_v3_typed_peer() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("typed-v3.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(
            matches!(read_frame::<ControlMsg, _>(&mut stream).await.unwrap(),
            ControlMsg::Execution(request) if matches!(*request, ExecutionRequest::Handshake(
                ExecutionHandshake { required_contract: 4, mode: ExecutionProtocolMode::RegisteredV3, .. })))
        );
        write_frame(
            &mut stream,
            &ControlReply::Execution(Box::new(ExecutionReply::Handshake {
                envelope_version: 3,
                executor_boot: ExecutorBootId::fresh().unwrap(),
                mode: ExecutionProtocolMode::RegisteredV3,
            })),
        )
        .await
        .unwrap();
    });
    assert!(
        RegisteredExecutorConnection::connect(&path, CapabilityToken::fresh().unwrap())
            .await
            .is_err()
    );
    peer.await.unwrap();
}
