use super::*;
use serde_json::json;

/// `RewindArm` as the binary before #2512 reads it, kept as that release's shape.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
enum PreviousReader {
    Codex { before_turn_id: String },
    Claude { resume_at: Uuid, drops_turn: Uuid },
}

/// A rolled-back binary still reads a cut this one wrote, and an unapplied cut keeps its old shape.
#[test]
fn a_recorded_revert_stays_readable_by_the_previous_binary() {
    let unapplied = BackendRewind(RewindArm::Codex {
        before_turn_id: "turn-2".into(),
        reverted: false,
    });
    let applied = BackendRewind(RewindArm::Codex {
        before_turn_id: "turn-2".into(),
        reverted: true,
    });
    assert_eq!(
        serde_json::to_value(&unapplied).unwrap(),
        json!({"provider": "codex", "before_turn_id": "turn-2"})
    );
    let written = serde_json::to_value(&applied).unwrap();
    assert_eq!(
        serde_json::from_value::<PreviousReader>(written.clone()).unwrap(),
        PreviousReader::Codex {
            before_turn_id: "turn-2".into()
        }
    );
    assert_eq!(
        serde_json::from_value::<BackendRewind>(written).unwrap(),
        applied
    );
    assert_eq!(
        serde_json::from_value::<BackendRewind>(
            json!({"provider": "codex", "before_turn_id": "turn-2"})
        )
        .unwrap(),
        unapplied
    );
}
