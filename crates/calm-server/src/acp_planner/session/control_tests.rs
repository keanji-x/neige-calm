//! Source-owner interrupt checks; no native execution or receipt fixture.
#![cfg(test)]
use super::*;
use provider::control::{InterruptOutcome, SubmissionEvidence};

#[test]
fn m1_mapping_acp_interrupt_reports_watch_delivery_not_native_ack() {
    for receiver_alive in [false, true] {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let mut receiver = Some(rx);
        if !receiver_alive {
            receiver.take();
        }
        match cancel_outcome(tx.send(true)) {
            InterruptOutcome::LocalSignalDelivered => assert!(receiver_alive),
            InterruptOutcome::Unconfirmed { error, evidence } => {
                assert!(!receiver_alive);
                assert!(error.0);
                assert_eq!(evidence, SubmissionEvidence::NotWritten);
            }
            other => panic!("wrong mapping: {other:?}"),
        }
        if let Some(rx) = receiver {
            assert!(*rx.borrow());
        }
    }
}

#[tokio::test]
async fn acp_identical_local_turns_keep_two_owner_cancellation_separate() {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let host = Arc::new(AcpPlannerHost::unconfigured_scratch().unwrap());
    let seals = Arc::new(ThreadSeals::new());
    let make = |owner: &str| {
        AcpPlannerSession::open(SessionParams {
            host: host.clone(),
            provider: AgentProvider::OpenCode,
            repo: repo.clone(),
            seals: seals.clone(),
            worker_session_id: owner.into(),
            card_id: owner.into(),
            track_id: owner.into(),
            cwd: "/tmp".into(),
            instructions: String::new(),
        })
    };
    let a = make("a");
    let b = make("b");
    let (a_cancel, a_seen) = watch::channel(false);
    let (b_cancel, b_seen) = watch::channel(false);
    a.shared.state.lock().unwrap().active = Some(Active {
        thread: "a".into(),
        turn: "same-local-turn".into(),
        cancel: a_cancel,
    });
    b.shared.state.lock().unwrap().active = Some(Active {
        thread: "b".into(),
        turn: "same-local-turn".into(),
        cancel: b_cancel,
    });
    assert!(matches!(
        a.turn_interrupt("b", "same-local-turn").await,
        InterruptOutcome::NoTarget
    ));
    assert!(matches!(
        a.turn_interrupt("a", "other-turn").await,
        InterruptOutcome::NoTarget
    ));
    assert!(!*a_seen.borrow());
    assert!(!*b_seen.borrow());
    assert!(matches!(
        a.turn_interrupt("a", "same-local-turn").await,
        InterruptOutcome::LocalSignalDelivered
    ));
    assert!(*a_seen.borrow());
    assert!(
        !*b_seen.borrow(),
        "identical local turn must not give authority over another owner"
    );
}
