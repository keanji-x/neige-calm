use super::*;

#[tokio::test]
async fn execution_launch_recovers_exact_reservation_after_journal_failure() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("retry", false);
    let grant = authority();
    let (scope, registration) = external(root, &grant);
    let request = prepare(
        &client,
        scope,
        &registration,
        grant,
        "retry",
        IoMode::Pipe,
        "printf once",
    );
    assert!(matches!(
        submit(&mut client, request.clone()).await,
        ExecutionReply::Prepared
    ));
    assert!(matches!(
        client
            .request(ExecutionRequest::Retire {
                execution_id: request.identity.execution_id.clone(),
                lifecycle: request.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap(),
        ExecutionReply::Busy(BusyReason::OutputOpen)
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
    let request = prepare(
        &client,
        scope,
        &registration,
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
