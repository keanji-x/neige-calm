use super::execution_support as support;
use calm_session::control::{IoMode, ProcSignal, execution::*};
use support::*;

#[tokio::test]
async fn execution_pipe_and_pty_use_registered_cwd_and_explicit_environment() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    for (index, mode) in [IoMode::Pipe, IoMode::Pty { cols: 80, rows: 24 }]
        .into_iter()
        .enumerate()
    {
        let root = harness.root(&format!("worktree-{index}"), true);
        let expected = root.path.display().to_string();
        let authority = authority();
        let (scope, registration) = external(root, &authority);

        let request = prepare(
            &client,
            scope,
            &registration,
            authority,
            &format!("cwd-{index}"),
            mode,
            "/bin/pwd; printf 'ambient:%s\\n' \"${NEIGE_EXECUTION_AMBIENT_SENTINEL-unset}\"",
        );
        let receipt = launch(&mut client, &request).await;
        let (_, output) = wait_for(&mut client, &request, &receipt, |state, output| {
            matches!(state, ExecutionState::Drained { .. }) && output.contains("ambient:unset")
        })
        .await;
        assert!(
            output.contains(&expected),
            "wrong actual child cwd: {output}"
        );
        let journal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(harness.journal().join(format!(
                "{}.execution",
                request.identity.execution_id.as_str()
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            journal["phase"]["Drained"]["proof"]["kernel_state"],
            "NoChildrenAfterForkEntryClosed"
        );
    }
}

#[tokio::test]
async fn execution_wrong_ticket_and_revoke_create_no_target() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("external", false);
    let marker = root.path.join("created");
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let request = prepare(
        &client,
        scope,
        &registration,
        authority,
        "revoked",
        IoMode::Pipe,
        "printf target > created",
    );
    assert!(matches!(
        submit(&mut client, request.clone()).await,
        ExecutionReply::Prepared
    ));
    let mut wrong = consume(&request);
    wrong.ticket = CapabilityToken::fresh().unwrap();
    assert!(matches!(
        client
            .request(ExecutionRequest::Launch(wrong))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Revoke(consume(&request)))
            .await
            .unwrap(),
        ExecutionReply::RevocationAcknowledged {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Launch(consume(&request)))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::Revoked,
            ..
        }
    ));
    assert!(!marker.exists());
}

#[tokio::test]
async fn execution_exact_ticket_retry_and_old_controls_cannot_touch_alias_replacement() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("worktree", true);
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let old = prepare(
        &client,
        scope.clone(),
        &registration,
        authority.clone(),
        "stable-alias",
        IoMode::Pipe,
        "printf old",
    );
    let old_receipt = launch(&mut client, &old).await;
    wait_for(&mut client, &old, &old_receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("old")
    })
    .await;
    let mut next = prepare(
        &client,
        scope,
        &registration,
        authority,
        "stable-alias",
        IoMode::Pipe,
        "printf ready; read value; printf 'new:%s' \"$value\"",
    );
    next.handoff = Some(AliasHandoff {
        previous_execution: old.identity.execution_id.clone(),
        expected_slot_version: old_receipt.slot_version,
    });
    let next_receipt = launch(&mut client, &next).await;
    wait_for(&mut client, &next, &next_receipt, |_, output| {
        output.contains("ready")
    })
    .await;
    match client
        .request(ExecutionRequest::Launch(consume(&old)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            ..
        } => assert_eq!(receipt, old_receipt),
        other => panic!("original retry lost: {other:?}"),
    }
    for action in [ProcessAction::Signal(ProcSignal::Kill)] {
        let reply = client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&old, &old_receipt, ExecutionControlPurpose::Lifecycle),
                action,
            }))
            .await
            .unwrap();
        assert!(matches!(
            reply,
            ExecutionReply::Refused { .. } | ExecutionReply::ActionComplete
        ));
    }
    loop {
        match client
            .request(ExecutionRequest::Retire {
                execution_id: old.identity.execution_id.clone(),
                lifecycle: old.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap()
        {
            ExecutionReply::Retired(_) => break,
            ExecutionReply::Busy(BusyReason::OutputOpen) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            other => panic!("old execution retire failed: {other:?}"),
        }
    }
    for action in [
        ProcessAction::WriteStdin {
            bytes: b"old-input\n".to_vec(),
        },
        ProcessAction::Resize {
            cols: 1,
            rows: 1,
            pixel_w: 0,
            pixel_h: 0,
        },
    ] {
        assert!(matches!(
            client
                .request(ExecutionRequest::Control(ExecutionControl {
                    capability: capability(
                        &old,
                        &old_receipt,
                        ExecutionControlPurpose::Interactive
                    ),
                    action
                }))
                .await
                .unwrap(),
            ExecutionReply::Refused { .. }
        ));
    }
    let mut wrong = capability(&next, &next_receipt, ExecutionControlPurpose::Observe);
    wrong.identity.source_registration = ObjectRegistrationId::fresh().unwrap();
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: wrong,
                action: ProcessAction::Observe { from_cursor: None },
            }))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    let mut forged = capability(&next, &next_receipt, ExecutionControlPurpose::Observe);
    forged.purpose = ExecutionControlPurpose::Lifecycle;
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: forged,
                action: ProcessAction::Signal(ProcSignal::Kill)
            }))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&next, &next_receipt, ExecutionControlPurpose::Interactive),
                action: ProcessAction::WriteStdin {
                    bytes: b"hello\n".to_vec()
                }
            }))
            .await
            .unwrap(),
        ExecutionReply::StdinProgress { accepted_bytes: 6 }
    ));
    wait_for(&mut client, &next, &next_receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("new:hello")
    })
    .await;
}

#[tokio::test]
async fn execution_worktree_external_alias_race_reserves_only_one_scope() {
    let harness = Harness::start().await;
    let mut left_client = harness.connect().await;
    let mut right_client = harness.connect().await;
    let left = harness.root("left", true);
    let right = harness.root("right", false);
    let left_marker = left.path.join("created");
    let right_marker = right.path.join("created");
    let authority = authority();
    let (left_scope, left_registration) = external(left, &authority);
    let (right_scope, right_registration) = external(right, &authority);

    let left = prepare(
        &left_client,
        left_scope,
        &left_registration,
        authority.clone(),
        "contended-alias",
        IoMode::Pipe,
        "printf left > created; sleep 0.1",
    );
    let right = prepare(
        &right_client,
        right_scope,
        &right_registration,
        authority,
        "contended-alias",
        IoMode::Pty { cols: 80, rows: 24 },
        "printf right > created; sleep 0.1",
    );
    assert!(matches!(
        submit(&mut left_client, left.clone()).await,
        ExecutionReply::Prepared
    ));
    assert!(matches!(
        submit(&mut right_client, right.clone()).await,
        ExecutionReply::Prepared
    ));
    let (a, b) = tokio::join!(
        left_client.request(ExecutionRequest::Launch(consume(&left))),
        right_client.request(ExecutionRequest::Launch(consume(&right)))
    );
    match (a.unwrap(), b.unwrap()) {
        (
            ExecutionReply::Receipt {
                receipt: Some(receipt),
                ..
            },
            ExecutionReply::Refused { .. },
        ) => {
            wait_for(&mut left_client, &left, &receipt, |state, _| {
                matches!(state, ExecutionState::Drained { .. })
            })
            .await;
            assert!(left_marker.exists() && !right_marker.exists());
        }
        (
            ExecutionReply::Refused { .. },
            ExecutionReply::Receipt {
                receipt: Some(receipt),
                ..
            },
        ) => {
            wait_for(&mut right_client, &right, &receipt, |state, _| {
                matches!(state, ExecutionState::Drained { .. })
            })
            .await;
            assert!(!left_marker.exists() && right_marker.exists());
        }
        other => panic!("alias admitted two or no scopes: {other:?}"),
    }
}

#[tokio::test]
async fn execution_source_replacement_and_stale_epoch_refuse_before_fork() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("original", true);
    let path = root.path.clone();
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let mut stale = prepare(
        &client,
        scope.clone(),
        &registration,
        authority.clone(),
        "stale",
        IoMode::Pipe,
        "printf created > target",
    );
    if let ExecutionScope::ExternalDir {
        registration_version: install_epoch,
        ..
    } = &mut stale.identity.scope
    {
        *install_epoch += 1;
    }
    assert!(matches!(
        submit(&mut client, stale).await,
        ExecutionReply::Refused { .. }
    ));
    std::fs::rename(&path, harness.temp.path().join("moved")).unwrap();
    harness.root("original", true);
    let replaced = prepare(
        &client,
        scope,
        &registration,
        authority,
        "replaced",
        IoMode::Pipe,
        "printf created > target",
    );
    assert!(matches!(
        submit(&mut client, replaced).await,
        ExecutionReply::Refused { .. }
    ));
    assert!(!path.join("target").exists());
}

#[tokio::test]
async fn execution_leader_exit_does_not_drain_setsiding_descendant() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("scope", false);
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let first = prepare(
        &client,
        scope.clone(),
        &registration,
        authority.clone(),
        "descendant-alias",
        IoMode::Pipe,
        "setsid /bin/sh -c 'sleep 0.7' & exit 0",
    );
    let receipt = launch(&mut client, &first).await;
    wait_for(&mut client, &first, &receipt, |state, _| {
        matches!(state, ExecutionState::Exited { .. })
    })
    .await;
    let mut next = prepare(
        &client,
        scope,
        &registration,
        authority,
        "descendant-alias",
        IoMode::Pipe,
        "printf next",
    );
    next.handoff = Some(AliasHandoff {
        previous_execution: first.identity.execution_id.clone(),
        expected_slot_version: receipt.slot_version,
    });
    assert!(matches!(
        submit(&mut client, next.clone()).await,
        ExecutionReply::Prepared
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Launch(consume(&next)))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    wait_for(&mut client, &first, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    let next_receipt = match client
        .request(ExecutionRequest::Launch(consume(&next)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
        } => receipt,
        other => panic!("drained handoff refused: {other:?}"),
    };
    wait_for(&mut client, &next, &next_receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("next")
    })
    .await;
}

#[tokio::test]
async fn execution_completed_journal_replays_original_receipt_after_supervisor_restart() {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("worktree", true);
    let counter = root.path.join("counter");
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let request = prepare(
        &client,
        scope,
        &registration,
        authority,
        "persisted-alias",
        IoMode::Pipe,
        "printf x >> counter",
    );
    let receipt = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(client);
    unsafe {
        libc::kill(harness.child.id() as i32, libc::SIGTERM);
    }
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    let mut client = harness.connect().await;
    assert_ne!(client.executor_boot(), &receipt.identity.executor_boot);
    match client
        .request(ExecutionRequest::Launch(consume(&request)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(original),
            state: ExecutionState::Drained { .. },
        } => assert_eq!(original, receipt),
        other => panic!("durable retry changed result: {other:?}"),
    }
    assert_eq!(std::fs::read(counter).unwrap(), b"x");
}

#[tokio::test]
async fn execution_launch_revoke_share_gate_and_ticket_is_single_use() {
    let harness = Harness::start().await;
    let mut a = harness.connect().await;
    let mut b = harness.connect().await;
    let root = harness.root("race", false);
    let marker = root.path.join("created");
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let request = prepare(
        &a,
        scope,
        &registration,
        authority,
        "launch-revoke-race",
        IoMode::Pipe,
        "printf x >> created; sleep 0.1",
    );
    assert!(matches!(
        submit(&mut a, request.clone()).await,
        ExecutionReply::Prepared
    ));
    let (launched, revoked) = tokio::join!(
        a.request(ExecutionRequest::Launch(consume(&request))),
        b.request(ExecutionRequest::Revoke(consume(&request)))
    );
    match (launched.unwrap(), revoked.unwrap()) {
        (
            ExecutionReply::Receipt {
                state: ExecutionState::Revoked,
                ..
            },
            ExecutionReply::RevocationAcknowledged {
                state: ExecutionState::Revoked,
                ..
            },
        ) => assert!(!marker.exists()),
        (
            ExecutionReply::Receipt {
                receipt: Some(receipt),
                state: ExecutionState::Spawned,
            },
            ExecutionReply::RevocationAcknowledged {
                receipt: Some(same),
                ..
            },
        ) => {
            assert_eq!(receipt, same);
            wait_for(&mut a, &request, &receipt, |state, _| {
                matches!(state, ExecutionState::Drained { .. })
            })
            .await;
            assert_eq!(std::fs::read(marker).unwrap(), b"x");
        }
        other => panic!("revoke/fork did not linearize: {other:?}"),
    }
}

#[tokio::test]
async fn execution_revocation_blocks_input_but_lifecycle_drains_exact_scope() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("closed-binding", true);
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let request = prepare(
        &client,
        scope,
        &registration,
        authority,
        "closing-execution",
        IoMode::Pty { cols: 80, rows: 24 },
        "printf ready; read value",
    );
    let receipt = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |_, output| {
        output.contains("ready")
    })
    .await;
    client
        .request(ExecutionRequest::Revoke(consume(&request)))
        .await
        .unwrap();
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&request, &receipt, ExecutionControlPurpose::Interactive),
                action: ProcessAction::WriteStdin {
                    bytes: b"must-not-enter\n".to_vec()
                }
            }))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&request, &receipt, ExecutionControlPurpose::Lifecycle),
                action: ProcessAction::Signal(ProcSignal::Kill)
            }))
            .await
            .unwrap(),
        ExecutionReply::ActionComplete
    ));
    wait_for(&mut client, &request, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
}

#[tokio::test]
async fn execution_legacy_alias_control_and_unregistered_client_are_refused() {
    let harness = Harness::start().await;
    assert!(
        RegisteredExecutorConnection::connect(&harness.socket, CapabilityToken::fresh().unwrap())
            .await
            .is_err()
    );
    let mut client = harness.connect().await;
    let root = harness.root("typed", false);
    let authority = authority();
    let (scope, registration) = external(root, &authority);

    let request = prepare(
        &client,
        scope,
        &registration,
        authority,
        "typed-only",
        IoMode::Pipe,
        "printf ready; read value",
    );
    let receipt = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |_, output| {
        output.contains("ready")
    })
    .await;
    // The real legacy client has no kernel secret and can never gain registered authority.
    let mut legacy = connect_legacy(&harness.socket).await.unwrap();
    for message in [
        calm_session::control::ControlMsg::Signal(calm_session::control::SignalRequest {
            proc_id: request.identity.process_alias.clone(),
            sig: ProcSignal::Kill,
        }),
        calm_session::control::ControlMsg::Probe(calm_session::control::ProbeRequest {
            proc_id: request.identity.process_alias.clone(),
        }),
    ] {
        calm_session::write_frame(&mut legacy, &message)
            .await
            .unwrap();
        assert!(matches!(
            calm_session::read_frame::<calm_session::control::ControlReply, _>(&mut legacy)
                .await
                .unwrap(),
            calm_session::control::ControlReply::Error {
                kind: calm_session::control::ControlErrorKind::BadRequest,
                ..
            }
        ));
    }
    client
        .request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&request, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: b"done\n".to_vec(),
            },
        }))
        .await
        .unwrap();
    wait_for(&mut client, &request, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
}

#[tokio::test]
async fn execution_id_and_ticket_reservations_are_global_across_resources() {
    let harness = Harness::start().await;
    let mut first_client = harness.connect().await;
    let mut second_client = harness.connect().await;
    let grant = authority();
    let (first_scope, first_registration) = external(harness.root("registered", true), &grant);
    let (second_scope, second_registration) = external(harness.root("external", false), &grant);

    for duplicate_execution in [true, false] {
        let first = prepare(
            &first_client,
            first_scope.clone(),
            &first_registration,
            grant.clone(),
            &format!("first-{duplicate_execution}"),
            IoMode::Pipe,
            "printf original",
        );
        let mut second = prepare(
            &second_client,
            second_scope.clone(),
            &second_registration,
            grant.clone(),
            &format!("second-{duplicate_execution}"),
            IoMode::Pipe,
            "printf original",
        );
        if duplicate_execution {
            second.identity.execution_id = first.identity.execution_id.clone();
        } else {
            second.identity.ticket_id = first.identity.ticket_id.clone();
        }
        let (first_reply, second_reply) = tokio::join!(
            async { Ok::<_, std::io::Error>(submit(&mut first_client, first.clone()).await) },
            async { Ok::<_, std::io::Error>(submit(&mut second_client, second.clone()).await) }
        );
        let (winner, loser) = match (first_reply.unwrap(), second_reply.unwrap()) {
            (ExecutionReply::Prepared, ExecutionReply::Refused { .. }) => (&first, &second),
            (ExecutionReply::Refused { .. }, ExecutionReply::Prepared) => (&second, &first),
            other => panic!("global identity reservation admitted both inputs: {other:?}"),
        };
        let stored: serde_json::Value = serde_json::from_slice(
            &std::fs::read(harness.journal().join(format!(
                "{}.execution",
                winner.identity.execution_id.as_str()
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(stored["request"], serde_json::to_value(winner).unwrap());
        assert!(matches!(
            first_client
                .request(ExecutionRequest::Launch(consume(loser)))
                .await
                .unwrap(),
            ExecutionReply::Refused { .. }
        ));
        let receipt = match first_client
            .request(ExecutionRequest::Launch(consume(winner)))
            .await
            .unwrap()
        {
            ExecutionReply::Receipt {
                receipt: Some(receipt),
                state: ExecutionState::Spawned,
            } => receipt,
            other => panic!("reserved winner did not launch: {other:?}"),
        };
        wait_for(&mut first_client, winner, &receipt, |state, output| {
            matches!(state, ExecutionState::Drained { .. }) && output.contains("original")
        })
        .await;
    }
}

#[path = "execution_contract/close_stdin.rs"]
mod close_stdin;
