use super::*;
use provider::control::{InterruptOutcome, SubmissionEvidence};
use std::process::Stdio;

#[tokio::test]
async fn m1_mapping_claude_interrupt_arms_timer_even_when_local_write_fails() {
    for alive in [true, false] {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let slot = TurnSlot::new(child.stdin.take().unwrap(), Duration::from_secs(1));
        if !alive {
            child.kill().await.unwrap();
        }
        match interrupt(&slot).await {
            InterruptOutcome::LocalSignalDelivered => assert!(alive),
            InterruptOutcome::Unconfirmed { error, evidence } => {
                assert!(!alive);
                assert!(matches!(error, CalmError::Io(_)));
                assert_eq!(evidence, SubmissionEvidence::WriteMayHaveReachedPeer);
            }
            other => panic!("wrong mapping: {other:?}"),
        }
        assert!(slot.stop_armed());
        assert!(matches!(slot.cause(), Some(TerminalCause::Interrupted)));
        if alive {
            child.kill().await.unwrap();
        }
    }
}
