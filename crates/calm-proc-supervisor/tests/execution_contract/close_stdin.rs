use super::*;

async fn action(
    client: &mut RegisteredExecutorConnection,
    capability: ExecutionCapability,
    action: ProcessAction,
) -> ExecutionReply {
    client
        .request(ExecutionRequest::Control(ExecutionControl {
            capability,
            action,
        }))
        .await
        .unwrap()
}

async fn refuse_bad_capabilities(
    c: &mut RegisteredExecutorConnection,
    r: &PrepareExecution,
    receipt: &ExecutionReceipt,
) {
    let cap = capability(r, receipt, ExecutionControlPurpose::Interactive);
    let mut bad = vec![cap.clone(); 4];
    bad[0].token = CapabilityToken::fresh().unwrap();
    bad[1].slot_version += 1;
    bad[2].identity.process_alias = "wrong".into();
    bad[3].identity.execution_id = ExecutionId::fresh().unwrap();
    bad.push(capability(r, receipt, ExecutionControlPurpose::Lifecycle));
    bad.push(capability(r, receipt, ExecutionControlPurpose::Observe));
    for cap in bad {
        assert!(c.close_stdin(cap).await.is_err());
    }
}

#[tokio::test]
async fn c2a_close_unblocks_eof_preserves_output_and_exact_retries() {
    let h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("eof", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "eof",
        IoMode::Pipe,
        "printf ready; cat; printf after-eof; sleep 0.2; printf done",
    );
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |state, out| {
        assert!(!matches!(state, ExecutionState::Drained { .. }));
        out.contains("ready")
    })
    .await;
    let cap = capability(&r, &receipt, ExecutionControlPurpose::Interactive);
    assert!(matches!(
        action(
            &mut c,
            cap.clone(),
            ProcessAction::WriteStdin {
                bytes: b"prefix".to_vec(),
            }
        )
        .await,
        ExecutionReply::StdinProgress { accepted_bytes: 6 }
    ));
    c.close_stdin(cap.clone()).await.unwrap();
    // Reconnect after a possible lost ACK: same boot, same identity, no stdin reopening.
    drop(c);
    let mut c = h.connect().await;
    c.close_stdin(cap.clone()).await.unwrap();
    for bytes in [vec![], b"late".to_vec()] {
        assert!(matches!(
            action(&mut c, cap.clone(), ProcessAction::WriteStdin { bytes }).await,
            ExecutionReply::Refused { .. }
        ));
    }
    let (_, out) = wait_for(&mut c, &r, &receipt, |state, out| {
        matches!(state, ExecutionState::Drained { .. }) && out.contains("done")
    })
    .await;
    assert_eq!(out, "readyprefixafter-eofdone");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if matches!(
                action(
                    &mut c,
                    capability(&r, &receipt, ExecutionControlPurpose::Observe),
                    ProcessAction::Observe { from_cursor: None }
                )
                .await,
                ExecutionReply::Observation {
                    output_end: OutputEnd::Complete { .. },
                    ..
                }
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_root_released(
        &h,
        match &registration {
            ExecutionRegistration::ExternalDir { root, .. } => root,
            _ => unreachable!(),
        },
    );
    c.close_stdin(cap.clone()).await.unwrap();
    refuse_bad_capabilities(&mut c, &r, &receipt).await;
    assert!(matches!(
        c.request(ExecutionRequest::Retire {
            execution_id: r.identity.execution_id.clone(),
            lifecycle: r.control_tokens.lifecycle.clone(),
        })
        .await
        .unwrap(),
        ExecutionReply::Retired(_)
    ));
    assert!(c.close_stdin(cap).await.is_err());
}

#[tokio::test]
async fn c2a_close_after_leader_exit_reaches_descendant() {
    let h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("descendant", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "descendant",
        IoMode::Pipe,
        "exec 3<&0; setsid /bin/sh -c 'cat; printf descendant-eof' <&3 3<&- & exit 0",
    );
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Exited { .. })
    })
    .await;
    let cap = capability(&r, &receipt, ExecutionControlPurpose::Interactive);
    refuse_bad_capabilities(&mut c, &r, &receipt).await;
    c.close_stdin(cap.clone()).await.unwrap();
    c.close_stdin(cap).await.unwrap();
    wait_for(&mut c, &r, &receipt, |state, out| {
        matches!(state, ExecutionState::Drained { .. }) && out.contains("descendant-eof")
    })
    .await;
}

#[tokio::test]
async fn c2a_close_refuses_pty_wrong_capabilities_and_revocation() {
    let h = Harness::start().await;
    let mut c = h.connect().await;
    for mode in [IoMode::Pipe, IoMode::Pty { cols: 80, rows: 24 }] {
        let grant = authority();
        let (scope, registration) = external(h.root(&format!("{mode:?}"), false), &grant);
        let r = prepare(
            &c,
            scope,
            &registration,
            grant,
            &format!("{mode:?}"),
            mode.clone(),
            "printf ready; read value; printf still-open",
        );
        let receipt = launch(&mut c, &r).await;
        wait_for(&mut c, &r, &receipt, |_, out| out.contains("ready")).await;
        let cap = capability(&r, &receipt, ExecutionControlPurpose::Interactive);
        if matches!(mode, IoMode::Pty { .. }) {
            assert!(c.close_stdin(cap.clone()).await.is_err());
        }
        refuse_bad_capabilities(&mut c, &r, &receipt).await;
        // Refusals did not close or disturb the real input.
        assert!(matches!(
            action(
                &mut c,
                cap.clone(),
                ProcessAction::WriteStdin {
                    bytes: b"ok\n".to_vec(),
                }
            )
            .await,
            ExecutionReply::StdinProgress { accepted_bytes: 3 }
        ));
        wait_for(&mut c, &r, &receipt, |state, out| {
            matches!(state, ExecutionState::Drained { .. }) && out.contains("still-open")
        })
        .await;
        if matches!(mode, IoMode::Pty { .. }) {
            assert!(c.close_stdin(cap.clone()).await.is_err());
        }
        c.request(ExecutionRequest::Revoke(consume(&r)))
            .await
            .unwrap();
        assert!(c.close_stdin(cap).await.is_err());
    }
}

#[tokio::test]
async fn c2a_close_refuses_unknown_after_supervisor_restart() {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("restart", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "restart",
        IoMode::Pipe,
        "printf ready; cat; printf eof",
    );
    // Before launch there is no process receipt; even a fabricated slot must not ACK.
    assert!(matches!(
        submit(&mut c, r.clone()).await,
        ExecutionReply::Prepared
    ));
    let fabricated = ExecutionCapability {
        identity: r.identity.clone(),
        slot_version: 1,
        token: r.control_tokens.interactive.clone(),
        purpose: ExecutionControlPurpose::Interactive,
    };
    assert!(c.close_stdin(fabricated).await.is_err());
    let receipt = match c
        .request(ExecutionRequest::Launch(consume(&r)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            ..
        } => receipt,
        other => panic!("{other:?}"),
    };
    wait_for(&mut c, &r, &receipt, |_, out| out.contains("ready")).await;
    let cap = capability(&r, &receipt, ExecutionControlPurpose::Interactive);
    drop(c);
    // Abrupt death leaves an unconfirmed journal, so restart has no native input handle.
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert!(matches!(
        c.request(ExecutionRequest::Inspect(r.identity.execution_id.clone()))
            .await
            .unwrap(),
        ExecutionReply::Inspected {
            state: ExecutionState::Unknown { .. },
            ..
        }
    ));
    assert!(c.close_stdin(cap).await.is_err());
}

#[tokio::test]
async fn c2a_server_rejects_envelope_eight() {
    use calm_session::{
        control::{ControlMsg, ControlReply},
        read_frame, write_frame,
    };
    let h = Harness::start().await;
    let mut socket = tokio::net::UnixStream::connect(&h.socket).await.unwrap();
    write_frame(
        &mut socket,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: 8,
            client_secret: h.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        }))),
    )
    .await
    .unwrap();
    assert!(
        matches!(read_frame::<ControlReply, _>(&mut socket).await.unwrap(),
        ControlReply::Execution(r) if matches!(*r, ExecutionReply::Refused { .. }))
    );
}

#[tokio::test]
async fn c2a_close_refuses_revoked_exited_and_no_child() {
    let h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let root = h.root("no-child", false);
    let path = root.path.clone();
    let (scope, registration) = external(root, &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "no-child",
        IoMode::Pipe,
        "cat",
    );
    assert!(matches!(
        submit(&mut c, r.clone()).await,
        ExecutionReply::Prepared
    ));
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(
        c.request(ExecutionRequest::Launch(consume(&r)))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            receipt: None,
            state: ExecutionState::NoChild { .. }
        }
    ));
    assert!(
        c.close_stdin(ExecutionCapability {
            identity: r.identity.clone(),
            slot_version: 1,
            token: r.control_tokens.interactive.clone(),
            purpose: ExecutionControlPurpose::Interactive,
        })
        .await
        .is_err()
    );

    let grant = authority();
    let (scope, registration) = external(h.root("revoked-exited", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "revoked-exited",
        IoMode::Pipe,
        "exec 3<&0; /bin/sh -c 'cat' <&3 3<&- & exit 0",
    );
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Exited { .. })
    })
    .await;
    c.request(ExecutionRequest::Revoke(consume(&r)))
        .await
        .unwrap();
    assert!(
        c.close_stdin(capability(
            &r,
            &receipt,
            ExecutionControlPurpose::Interactive
        ))
        .await
        .is_err()
    );
    action(
        &mut c,
        capability(&r, &receipt, ExecutionControlPurpose::Lifecycle),
        ProcessAction::Stop,
    )
    .await;
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    refuse_bad_capabilities(&mut c, &r, &receipt).await;
    assert!(
        c.close_stdin(capability(
            &r,
            &receipt,
            ExecutionControlPurpose::Interactive
        ))
        .await
        .is_err()
    );
}
