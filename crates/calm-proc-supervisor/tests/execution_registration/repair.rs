use super::*;

#[tokio::test]
async fn execution_launch_recovers_exact_reservation_after_journal_failure() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("retry", false);
    let grant = authority();
    let (scope, registration) = external(root, &grant);
    register(&mut client, registration).await;
    let request = prepare(&client, scope, grant, "retry", IoMode::Pipe, "printf once");
    assert!(matches!(
        client
            .request(ExecutionRequest::Prepare(request.clone()))
            .await
            .unwrap(),
        ExecutionReply::Prepared
    ));
    let receipt = client.registrations()[0].clone();
    assert!(matches!(
        client
            .request(ExecutionRequest::RetireRegistration(receipt))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    let journal = harness.journal();
    let moved = harness.temp.path().join("moved-journal");
    std::fs::rename(&journal, &moved).unwrap();
    let failure = client
        .request(ExecutionRequest::Launch(consume(&request)))
        .await
        .unwrap();
    std::fs::rename(&moved, &journal).unwrap();
    assert!(matches!(failure, ExecutionReply::Refused { .. }));
    let receipt = launch_prepared(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output == "once"
    })
    .await;
    // Already durable historical ACKs do not need a fresh write to remain readable.
    std::fs::rename(&journal, &moved).unwrap();
    let replay = client
        .request(ExecutionRequest::Launch(consume(&request)))
        .await
        .unwrap();
    std::fs::rename(&moved, &journal).unwrap();
    assert!(matches!(replay, ExecutionReply::Receipt { receipt: Some(r), .. } if r == receipt));
}

#[tokio::test]
async fn execution_stdin_backpressure_reports_exact_accepted_prefix() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("input", false);
    let path = root.path.clone();
    let grant = authority();
    let (scope, registration) = external(root, &grant);
    register(&mut client, registration).await;
    let request = prepare(
        &client,
        scope,
        grant,
        "input",
        IoMode::Pipe,
        "printf ready; while [ ! -f read-now ]; do sleep 0.01; done; head -c 1048576 > received; printf done",
    );
    let receipt = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |_, output| {
        output == "ready"
    })
    .await;
    let bytes: Vec<u8> = (0..1048576).map(|i| (i % 251) as u8).collect();
    let response = input(&mut client, &request, &receipt, &bytes).await;
    let ExecutionReply::StdinProgress { accepted_bytes } = response else {
        panic!("input prefix lost: {response:?}")
    };
    assert!(accepted_bytes > 0 && accepted_bytes < bytes.len());
    assert!(matches!(
        input(&mut client, &request, &receipt, &bytes[accepted_bytes..]).await,
        ExecutionReply::StdinProgress { accepted_bytes: 0 }
    ));
    std::fs::write(path.join("read-now"), b"").unwrap();
    let mut offset = accepted_bytes;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while offset < bytes.len() {
        let response = input(
            &mut client,
            &request,
            &receipt,
            &bytes[offset..bytes.len().min(offset + 16384)],
        )
        .await;
        let ExecutionReply::StdinProgress { accepted_bytes } = response else {
            panic!("input progress lost: {response:?}")
        };
        offset += accepted_bytes;
        assert!(tokio::time::Instant::now() < deadline);
        if accepted_bytes == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    wait_for(&mut client, &request, &receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output == "readydone"
    })
    .await;
    assert_eq!(std::fs::read(path.join("received")).unwrap(), bytes);
}

#[tokio::test]
async fn execution_retirement_releases_only_exact_settled_lease_and_preserves_history() {
    let mut pair = pair().await;
    let first = pair.left.registrations()[0].clone();
    let second = pair.right.registrations()[0].clone();
    assert!(matches!(
        pair.right
            .request(ExecutionRequest::RetireRegistration(second.clone()))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    stop(&mut pair.right, &pair.second, &pair.second_receipt).await;
    assert!(
        matches!(pair.right.request(ExecutionRequest::RetireRegistration(second.clone())).await.unwrap(), ExecutionReply::RegistrationRetired { registration } if registration == second.id)
    );
    assert!(matches!(
        input(&mut pair.left, &pair.first, &pair.first_receipt, b"peer\n").await,
        ExecutionReply::StdinProgress { accepted_bytes: 5 }
    ));
    assert!(
        matches!(pair.right.request(ExecutionRequest::Launch(consume(&pair.second))).await.unwrap(), ExecutionReply::Receipt { receipt: Some(receipt), state: ExecutionState::Drained { .. } } if receipt == pair.second_receipt)
    );
    assert!(
        pair.right
            .attach(second.transfer.clone(), &pair.held)
            .await
            .is_err()
    );
    assert!(
        matches!(pair.right.request(ExecutionRequest::RetireRegistration(second.clone())).await.unwrap(), ExecutionReply::RegistrationRetired { registration } if registration == second.id)
    );
    let historical: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            pair.harness
                .journal()
                .join(format!("{}.registration", second.id.as_str())),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(historical["id"], second.id.as_str());
    stop(&mut pair.left, &pair.first, &pair.first_receipt).await;
    assert!(matches!(
        pair.left
            .request(ExecutionRequest::RetireRegistration(first.clone()))
            .await
            .unwrap(),
        ExecutionReply::RegistrationRetired { .. }
    ));
    let root = pair.held.root().metadata().unwrap();
    use std::os::unix::fs::MetadataExt;
    let held_count = std::fs::read_dir(format!("/proc/{}/fd", pair.harness.child.id()))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::metadata(entry.path()).ok())
        .filter(|metadata| metadata.dev() == root.dev() && metadata.ino() == root.ino())
        .count();
    assert_eq!(
        held_count, 0,
        "retirement left supervisor source descriptions pinned"
    );
    // New immutable operation grants can be admitted and retired repeatedly without descriptor growth.
    for _ in 0..24 {
        let mut transfer = second.transfer.clone();
        transfer.registration_id = ObjectRegistrationId::fresh().unwrap();
        transfer.nonce = TransferNonce::fresh().unwrap();
        let receipt = pair.left.attach(transfer, &pair.held).await.unwrap();
        assert!(matches!(
            pair.left
                .request(ExecutionRequest::RetireRegistration(receipt))
                .await
                .unwrap(),
            ExecutionReply::RegistrationRetired { .. }
        ));
    }
}

#[tokio::test]
async fn execution_retirement_waits_for_reverse_export_ack() {
    use calm_session::control::{ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("export", false);
    let grant = authority();
    let (_, registration) = external(root, &grant);
    register(&mut client, registration).await;
    let receipt = client.registrations()[0].clone();
    let mut stream = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    write_frame(
        &mut stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: EXECUTION_ENVELOPE_VERSION,
            client_secret: harness.secret.clone(),
            mode: ExecutionProtocolMode::RegisteredV3,
        }))),
    )
    .await
    .unwrap();
    let _: ControlReply = read_frame(&mut stream).await.unwrap();
    let nonce = TransferNonce::fresh().unwrap();
    rights::send(
        &stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::Export(ExportRegistration {
            executor_boot: client.executor_boot().clone(),
            registration: receipt.id.clone(),
            nonce: nonce.clone(),
        }))),
        &[],
    )
    .await
    .unwrap();
    let (response, files) = rights::receive::<ControlReply>(&stream).await.unwrap();
    assert!(
        matches!(response, ControlReply::Execution(reply) if matches!(*reply, ExecutionReply::Exported { .. }))
    );
    assert!(!files.is_empty());
    assert!(matches!(
        client
            .request(ExecutionRequest::RetireRegistration(receipt.clone()))
            .await
            .unwrap(),
        ExecutionReply::Refused { .. }
    ));
    rights::send(
        &stream,
        &ControlMsg::Execution(Box::new(ExecutionRequest::ExportAck(
            ExportAcknowledgement {
                executor_boot: client.executor_boot().clone(),
                registration: receipt.id.clone(),
                nonce,
            },
        ))),
        &[],
    )
    .await
    .unwrap();
    let _ = rights::receive::<ControlReply>(&stream).await.unwrap();
    drop(files);
    assert!(matches!(
        client
            .request(ExecutionRequest::RetireRegistration(receipt))
            .await
            .unwrap(),
        ExecutionReply::RegistrationRetired { .. }
    ));
}
