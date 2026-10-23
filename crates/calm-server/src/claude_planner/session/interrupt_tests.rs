use super::*;
use provider::control::SubmissionEvidence;

#[tokio::test]
async fn m1_mapping_claude_interrupt_arms_timer_even_when_local_write_fails() {
    for alive in [true, false] {
        let (stdin, peer) = tokio::io::duplex(4096);
        let slot = TurnSlot::new(stdin, Duration::from_secs(1));
        let _peer = if alive {
            Some(peer)
        } else {
            drop(peer);
            None
        };
        match interrupt(&slot, historical_admission().gate()).await {
            Ok(()) => assert!(alive),
            Err((error, evidence)) => {
                assert!(!alive);
                assert!(matches!(error, CalmError::Io(_)));
                assert_eq!(evidence, SubmissionEvidence::WriteMayHaveReachedPeer);
            }
        }
        assert!(slot.stop_armed());
        assert!(matches!(slot.cause(), Some(TerminalCause::Interrupted)));
    }
}
