use super::*;

#[tokio::test]
async fn dedicated_codex_replaced_native_socket_blocks_further_requests() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("mcp-epoch").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    std::fs::remove_file(&f.native.socket).unwrap();
    let _replacement = std::os::unix::net::UnixListener::bind(&f.native.socket).unwrap();
    assert!(matches!(
        session.create_thread(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert!(
        !Fixture::calls(&endpoint)
            .iter()
            .any(|call| call["method"] == "thread/start")
    );
}

#[tokio::test]
async fn dedicated_codex_stop_checkpoint_rejection_preserves_caller_record() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("stop-journal").await;
    let journal = Journal::default();
    let session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    let mut record = session.record().clone();
    let before = record.clone();
    drop(session);
    *journal.reject.lock().unwrap() = Some("stop");
    assert!(
        f.controller
            .stop(&mut record, &journal, Duration::from_secs(3))
            .await
            .is_err()
    );
    assert_eq!(record, before);
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Running
    );
    *journal.reject.lock().unwrap() = None;
    assert!(matches!(
        f.controller
            .stop(&mut record, &journal, Duration::from_secs(3))
            .await
            .unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[tokio::test]
async fn dedicated_codex_concurrent_controllers_issue_one_thread_request() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("concurrent").await;
    let journal = Journal::default();
    let mut first = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    let mut second = f
        .controller
        .connect(first.record().clone(), &journal)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        first.create_thread(&journal),
        second.create_thread(&journal)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        Fixture::calls(&endpoint)
            .iter()
            .filter(|call| call["method"] == "thread/start")
            .count(),
        1
    );
}

#[tokio::test]
async fn dedicated_codex_distinct_endpoints_reuse_context_without_shared_state() {
    let f = Fixture::new("normal");
    let journal = Journal::default();
    let a = f.prepare("a").await;
    let b = f.prepare("b").await;
    assert_ne!(a.home.socket, b.home.socket);
    assert_ne!(a.boundary.init, b.boundary.init);
    let mut first = f
        .controller
        .connect(journal.prepared(a.clone()), &journal)
        .await
        .unwrap();
    let mut second = f
        .controller
        .connect(journal.prepared(b.clone()), &journal)
        .await
        .unwrap();
    assert_eq!(first.create_thread(&journal).await.unwrap(), "owned-thread");
    assert_eq!(
        second.create_thread(&journal).await.unwrap(),
        "owned-thread"
    );
    first
        .begin_turn("turn-a", "task A", &journal, &Allow)
        .await
        .unwrap();
    second
        .begin_turn("turn-b", "task B", &journal, &Allow)
        .await
        .unwrap();
    for (endpoint, prompt) in [(a, "task A"), (b, "task B")] {
        let calls = Fixture::calls(&endpoint);
        let creation = calls
            .iter()
            .find(|call| call["method"] == "thread/start")
            .unwrap();
        assert_eq!(
            creation["params"]["developerInstructions"],
            "unchanged frozen worker context"
        );
        let turn = calls
            .iter()
            .find(|call| call["method"] == "turn/start")
            .unwrap();
        assert_eq!(turn["params"]["input"][0]["text"], prompt);
    }
    let mut events = first.take_notifications().unwrap();
    assert!(first.take_notifications().is_err());
    let item = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.thread_id(), Some("owned-thread"));
}

#[tokio::test]
async fn dedicated_codex_missing_or_denied_profile_never_starts_a_thread() {
    for scenario in ["missing-profile", "denied-profile", "missing-allowed"] {
        let f = Fixture::new(scenario);
        let endpoint = f.prepare("blocked").await;
        let journal = Journal::default();
        assert!(matches!(
            f.controller
                .connect(journal.prepared(endpoint.clone()), &journal)
                .await,
            Err(Error::Unsupported(_))
        ));
        assert!(
            !Fixture::calls(&endpoint)
                .iter()
                .any(|call| call["method"] == "thread/start")
        );
    }
}

#[tokio::test]
async fn dedicated_codex_wrong_created_context_never_begins_a_turn() {
    let f = Fixture::new("wrong-context");
    let endpoint = f.prepare("wrong-context").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    assert!(matches!(
        session.create_thread(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert!(
        session
            .begin_turn("no-turn", "prompt", &journal, &Allow)
            .await
            .is_err()
    );
    assert!(
        !Fixture::calls(&endpoint)
            .iter()
            .any(|call| call["method"] == "turn/start")
    );
}

#[tokio::test]
async fn dedicated_codex_start_checkpoint_refusal_leaves_provider_unstarted() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("not-started").await;
    let journal = Journal::default();
    *journal.reject.lock().unwrap() = Some("start");
    assert!(matches!(
        f.controller
            .connect(journal.prepared(endpoint.clone()), &journal)
            .await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Prepared
    );
    assert!(Fixture::calls(&endpoint).is_empty());
}

#[tokio::test]
async fn dedicated_codex_checkpoints_veto_thread_and_turn_requests() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("journal").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    *journal.reject.lock().unwrap() = Some("thread");
    assert!(matches!(
        session.create_thread(&journal).await,
        Err(Error::Conflict(_))
    ));
    assert!(
        !Fixture::calls(&endpoint)
            .iter()
            .any(|call| call["method"] == "thread/start")
    );
    *journal.reject.lock().unwrap() = None;
    session.create_thread(&journal).await.unwrap();
    *journal.reject.lock().unwrap() = Some("turn");
    assert!(matches!(
        session.begin_turn("turn", "prompt", &journal, &Allow).await,
        Err(Error::Conflict(_))
    ));
    assert!(
        !Fixture::calls(&endpoint)
            .iter()
            .any(|call| call["method"] == "turn/start")
    );
}

#[tokio::test]
async fn dedicated_codex_lost_thread_reply_reconciles_single_owned_thread() {
    let f = Fixture::new("lose-thread-reply");
    let endpoint = f.prepare("thread-loss").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    assert!(matches!(
        session.create_thread(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert_eq!(session.record().phase, RequestPhase::CreatingThread);
    let record = session.record().clone();
    drop(session);
    let mut resumed = f.controller.connect(record, &journal).await.unwrap();
    resumed.reconcile(&journal).await.unwrap();
    assert_eq!(
        resumed.create_thread(&journal).await.unwrap(),
        "owned-thread"
    );
    assert_eq!(
        Fixture::calls(&endpoint)
            .iter()
            .filter(|call| call["method"] == "thread/start")
            .count(),
        1
    );
}

#[tokio::test]
async fn dedicated_codex_lost_turn_reply_stays_unknown_without_replay() {
    let f = Fixture::new("lose-turn-reply");
    let endpoint = f.prepare("turn-loss").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    assert!(matches!(
        session
            .begin_turn("once", "unchanged prompt", &journal, &Allow)
            .await,
        Err(Error::Unknown(_))
    ));
    let record = session.record().clone();
    drop(session);
    let mut resumed = f.controller.connect(record, &journal).await.unwrap();
    assert!(matches!(
        resumed.reconcile(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert!(matches!(
        resumed
            .begin_turn("once", "unchanged prompt", &journal, &Allow)
            .await,
        Err(Error::Unknown(_))
    ));
    assert_eq!(
        Fixture::calls(&endpoint)
            .iter()
            .filter(|call| call["method"] == "turn/start")
            .count(),
        1
    );
}

#[tokio::test]
async fn dedicated_codex_reconnect_keeps_exact_turn_and_closed_stop_fence() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("resume").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    let turn = session
        .begin_turn("once", "prompt", &journal, &Allow)
        .await
        .unwrap();
    let record = session.record().clone();
    drop(session);
    let mut resumed = f.controller.connect(record, &journal).await.unwrap();
    resumed.reconcile(&journal).await.unwrap();
    assert_eq!(
        resumed
            .begin_turn("once", "prompt", &journal, &Allow)
            .await
            .unwrap(),
        turn
    );
    let mut record = resumed.record().clone();
    drop(resumed);
    assert!(matches!(
        f.controller
            .stop(&mut record, &journal, Duration::from_secs(3))
            .await
            .unwrap(),
        BoundaryState::Quiesced(_)
    ));
    assert!(matches!(
        f.controller.connect(record, &journal).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        Fixture::calls(&endpoint)
            .iter()
            .filter(|call| call["method"] == "turn/start")
            .count(),
        1
    );
}
