use super::*;
use std::path::Path;

async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake provider must reach the real socket boundary");
}

async fn cancelled_turn(caller_abort: bool) {
    let f = Fixture::with_config("backpressure", |config| {
        config.request_timeout = if caller_abort {
            Duration::from_secs(3)
        } else {
            Duration::from_millis(500)
        };
    });
    let endpoint = f.prepare("cancelled-control").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    let prompt = "x".repeat(8 * 1024 * 1024);
    let partial = endpoint.home.home.join("partial-bytes");
    if caller_abort {
        let sending = session.begin_turn("once", &prompt, &journal, &Allow);
        tokio::pin!(sending);
        tokio::select! {
            result = &mut sending => panic!("request ended before explicit caller cancellation: {result:?}"),
            () = wait_for(&partial) => {}
        }
        // Drop the actual controller future during the partly flushed issue.
    } else {
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                session.begin_turn("once", &prompt, &journal, &Allow)
            )
            .await
            .unwrap(),
            Err(Error::Unknown(_))
        ));
        wait_for(&partial).await;
    }
    let bytes: usize = std::fs::read_to_string(partial).unwrap().parse().unwrap();
    assert!(bytes > 0 && bytes < 1024 * 1024);
    assert!(matches!(
        session.record().phase,
        RequestPhase::IssuingTurn { .. }
    ));
    assert_eq!(session.record().endpoint, endpoint);
    let calls = Fixture::calls(&endpoint).len();
    assert!(matches!(
        session.reconcile(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert!(matches!(
        session.begin_turn("once", &prompt, &journal, &Allow).await,
        Err(Error::Unknown(_))
    ));
    assert_eq!(
        Fixture::calls(&endpoint).len(),
        calls,
        "uncertain phase must reject before incidental profile RPC"
    );
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Running
    );
    assert!(endpoint.home.home.is_dir() && endpoint.request.workspace.is_dir());
    std::fs::write(endpoint.home.home.join("release-reader"), b"go").unwrap();
    let business = endpoint.home.home.join("business-received");
    let closed = endpoint.home.home.join("connection-ended");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !business.exists() && !closed.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !business.exists(),
        "reader/Pong must not complete the old turn outside admission"
    );
    assert!(closed.exists());
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Running
    );
}

#[tokio::test]
async fn dedicated_codex_caller_cancel_keeps_unknown_owned_endpoint_without_late_send() {
    cancelled_turn(true).await;
}

#[tokio::test]
async fn dedicated_codex_send_deadline_keeps_unknown_owned_endpoint_without_late_send() {
    cancelled_turn(false).await;
}
