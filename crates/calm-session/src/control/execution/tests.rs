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
            ControlMsg::Execution(ExecutionRequest::Handshake(ExecutionHandshake {
                mode: ExecutionProtocolMode::RegisteredV2,
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
