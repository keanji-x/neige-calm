use super::*;

fn open(root: &std::path::Path, secret: &CapabilityToken) -> Arc<Executor> {
    Executor::open(ExecutorConfig {
        journal_directory: root.to_owned(),
        helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
        kernel_client_secret: secret.clone(),
    })
    .unwrap()
}

#[tokio::test]
async fn tombstone_resumes_journal_removal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let secret = CapabilityToken::fresh().unwrap();
    let tombstone = ExecutionTombstone {
        execution_id: ExecutionId::fresh().unwrap(),
        ticket_id: LaunchTicketId::fresh().unwrap(),
        registration: None,
        directory: None,
        slot: None,
        outcome: TombstoneOutcome::Revoked,
    };
    let name = format!("{}.execution", tombstone.execution_id.as_str());
    let tombstone_name = format!("{}.tombstone", tombstone.execution_id.as_str());
    // Crash immediately after tombstone fsync: the superseded journal must never be parsed.
    let store = Store::open(root.clone()).unwrap();
    std::fs::write(root.join(&name), b"malformed superseded secret journal").unwrap();
    store.write(&tombstone_name, &tombstone).unwrap();
    let original = std::fs::read(root.join(&tombstone_name)).unwrap();
    drop(store);
    std::fs::remove_file(root.join(&name)).unwrap();
    std::fs::create_dir(root.join(&name)).unwrap();
    assert!(
        Executor::open(ExecutorConfig {
            journal_directory: root.clone(),
            helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
            kernel_client_secret: secret.clone(),
        })
        .is_err()
    );
    std::fs::remove_dir(root.join(&name)).unwrap();
    std::fs::write(root.join(&name), b"malformed superseded secret journal").unwrap();
    let executor = open(&root, &secret);
    assert!(!root.join(&name).exists(), "startup must resume deletion");
    assert!(executor.records.lock().unwrap().is_empty());

    // Exercise production dispatch: each tombstone path must attempt cleanup on failure and retry.
    let (mut prepare, _held) = protocol_tests::private(&executor, temp.path());
    prepare.identity.execution_id = tombstone.execution_id.clone();
    prepare.identity.ticket_id = tombstone.ticket_id.clone();
    let mut reused_ticket = prepare.clone();
    reused_ticket.identity.execution_id = ExecutionId::fresh().unwrap();
    let consume = ConsumeExecution {
        execution_id: tombstone.execution_id.clone(),
        ticket_id: tombstone.ticket_id.clone(),
        ticket: CapabilityToken::fresh().unwrap(),
    };
    let requests = [
        ExecutionRequest::Inspect(tombstone.execution_id.clone()),
        ExecutionRequest::Retire {
            execution_id: tombstone.execution_id.clone(),
            lifecycle: CapabilityToken::fresh().unwrap(),
        },
        ExecutionRequest::Revoke(consume.clone()),
        ExecutionRequest::Launch(consume.clone()),
        ExecutionRequest::Revoke(ConsumeExecution {
            execution_id: ExecutionId::fresh().unwrap(),
            ..consume
        }),
        ExecutionRequest::Prepare(prepare),
        ExecutionRequest::Prepare(reused_ticket),
    ];
    for request in requests {
        std::fs::create_dir(root.join(&name)).unwrap();
        let (_, mut changes) = executor.watch().unwrap();
        assert!(executor.persist_tombstone(tombstone.clone()).is_err());
        assert_eq!(changes.try_recv().unwrap().1, tombstone.execution_id);
        let failed = executor.dispatch(&secret, request.clone()).await;
        assert!(
            matches!(failed, ExecutionReply::Refused { .. }),
            "{request:?}: {failed:?}"
        );
        assert!(root.join(&name).is_dir());
        std::fs::remove_dir(root.join(&name)).unwrap();
        std::fs::write(root.join(&name), b"still not a journal").unwrap();
        let reply = executor.dispatch(&secret, request.clone()).await;
        if matches!(&request, ExecutionRequest::Prepare(_))
            || matches!(&request, ExecutionRequest::Revoke(r) if r.execution_id != tombstone.execution_id)
        {
            assert!(matches!(reply, ExecutionReply::Refused { .. }), "{reply:?}");
        } else {
            assert!(
                matches!(reply, ExecutionReply::Retired(ref t) if *t == tombstone),
                "{request:?}: {reply:?}"
            );
        }
        assert!(
            !root.join(&name).exists(),
            "{request:?}: tombstone replay must resume deletion"
        );
        assert_eq!(std::fs::read(root.join(&tombstone_name)).unwrap(), original);
    }
    let mut conflicting = tombstone.clone();
    conflicting.outcome = TombstoneOutcome::Retired;
    assert!(executor.persist_tombstone(conflicting).is_err());
    assert_eq!(std::fs::read(root.join(&tombstone_name)).unwrap(), original);
    drop(executor);
    let executor = open(&root, &secret);
    assert!(
        matches!(executor.inspect(&tombstone.execution_id).unwrap(), ExecutionReply::Retired(t) if t == tombstone)
    );
    assert!(!root.join(&name).exists());
}
