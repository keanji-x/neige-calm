use super::*;
use tokio::sync::Notify;

struct Guard<'a> {
    journal: &'a Journal,
    entered: Notify,
    release: Notify,
    pause: bool,
    invoked: AtomicBool,
    withdrawn: AtomicBool,
}

#[async_trait::async_trait]
impl TurnAdmission for Guard<'_> {
    async fn admit(
        &self,
        launch: TurnLaunch,
    ) -> Result<calm_server::codex_appserver::TurnStartResult> {
        self.invoked.store(true, Ordering::SeqCst);
        assert!(matches!(
            self.journal.current.lock().unwrap()[&launch.endpoint().request.identity.run_id].phase,
            RequestPhase::IssuingTurn { .. }
        ));
        self.entered.notify_one();
        if self.pause {
            self.release.notified().await;
        }
        if self.withdrawn.load(Ordering::SeqCst) {
            return Err(Error::Conflict("withdrawn at final admission".into()));
        }
        assert_eq!(launch.thread_id(), "owned-thread");
        assert!(!launch.request_key().is_empty());
        assert_eq!(launch.prompt_digest().len(), 64);
        assert!(launch.control_timeout() <= Duration::from_secs(60));
        self.journal.inside_admission.store(true, Ordering::SeqCst);
        let result = launch.issue().await;
        self.journal.inside_admission.store(false, Ordering::SeqCst);
        result
    }
}

#[tokio::test]
async fn dedicated_codex_withdrawal_after_checkpoint_before_admission_sends_no_turn() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("admission-withdraw").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    let admission = Guard {
        journal: &journal,
        entered: Notify::new(),
        release: Notify::new(),
        pause: true,
        invoked: AtomicBool::new(false),
        withdrawn: AtomicBool::new(false),
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            session.begin_turn("withdrawn", "prompt", &journal, &admission),
            async {
                admission.entered.notified().await;
                assert!(matches!(
                    journal.current.lock().unwrap()["admission-withdraw"].phase,
                    RequestPhase::IssuingTurn { .. }
                ));
                admission.withdrawn.store(true, Ordering::SeqCst);
                admission.release.notify_one();
            }
        )
    })
    .await
    .expect("admission must occur after CAS and remain bounded");
    assert!(matches!(result, Err(Error::Conflict(_))));
    assert!(
        !Fixture::calls(&endpoint)
            .iter()
            .any(|call| call["method"] == "turn/start")
    );
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Running
    );
}

#[tokio::test]
async fn dedicated_codex_checkpoints_stay_outside_final_admission_guard() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("guard-order").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    let admission = Guard {
        journal: &journal,
        entered: Notify::new(),
        release: Notify::new(),
        pause: false,
        invoked: AtomicBool::new(false),
        withdrawn: AtomicBool::new(false),
    };
    session
        .begin_turn("guarded", "prompt", &journal, &admission)
        .await
        .unwrap();
    // Journal::save asserts that no callback ran while the simulated writer was held.
    assert!(admission.invoked.load(Ordering::SeqCst));
    assert!(matches!(
        journal.current.lock().unwrap()["guard-order"].phase,
        RequestPhase::TurnActive { .. }
    ));
    assert!(!journal.inside_admission.load(Ordering::SeqCst));
}

#[tokio::test]
async fn dedicated_codex_ack_checkpoint_failure_retains_owned_endpoint_without_retry() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("ack-save-failed").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    *journal.reject.lock().unwrap() = Some("turn-ack");
    assert!(matches!(
        session.begin_turn("once", "prompt", &journal, &Allow).await,
        Err(Error::Unknown(_))
    ));
    assert!(matches!(
        session.record().phase,
        RequestPhase::IssuingTurn { .. }
    ));
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Running
    );
    assert!(endpoint.home.home.is_dir());
    assert!(endpoint.request.workspace.is_dir());
    let record = session.record().clone();
    drop(session);
    *journal.reject.lock().unwrap() = None;
    let mut resumed = f.controller.connect(record, &journal).await.unwrap();
    assert!(matches!(
        resumed.begin_turn("once", "prompt", &journal, &Allow).await,
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
