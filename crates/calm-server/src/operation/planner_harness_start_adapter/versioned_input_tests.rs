use super::*;
use crate::harness::queue::{QueueMutation, apply_mutation, input_segments_for_entries};

fn journal_output(message: Value) -> TxOutput {
    let mut output = TxOutput::new("card", Some("journal-card".into()), json!({}));
    output.data = json!({"harvested_from":[{"worker_session_id":"source","messages":[message]}]});
    output
}

#[test]
fn versioned_input_journal_round_trips_the_actual_edited_revision() {
    let entry = QueueEntry::user_message("before transfer edit".into(), None, Vec::new());
    let id = entry.id().unwrap().clone();
    let mut queue = std::collections::VecDeque::from([entry]);
    let edited = apply_mutation(
        &mut queue,
        &QueueMutation::Edit {
            entry_id: id.clone(),
            if_entry_rev: 0,
            text: "actual transferred words 👋\n".into(),
        },
    )
    .unwrap();
    let actual = queue.pop_front().unwrap();
    let actual_text = actual.user_view().unwrap().text.to_string();
    let actual_ids = actual.message_ids().to_vec();
    let snapshot = HarnessSnapshot::initial(0, vec![actual]);
    let taken = stranded_user_messages("source", &serde_json::to_string(&snapshot).unwrap()).taken;
    let persisted = harvested_from_journal(&[HarvestedFrom {
        worker_session_id: "source".into(),
        messages: taken,
    }]);
    assert_eq!(persisted[0]["messages"][0]["entry_id"], id.as_str());
    assert_eq!(persisted[0]["messages"][0]["entry_rev"], edited.rev);
    let mut output = TxOutput::new("card", Some("journal-card".into()), json!({}));
    output.data = json!({"harvested_from":persisted});
    let decoded = read_harvested_from_journal(&output).unwrap();
    let message = &decoded[0].messages[0];
    let rebuilt = QueueEntry::user_message_moved(
        message.text.clone(),
        message.ids.clone(),
        message.entry_ref.clone(),
    );
    let view = rebuilt.user_view().unwrap();
    assert_eq!(view.id, &id);
    assert_eq!(view.rev, edited.rev);
    assert_eq!(view.text, actual_text);
    assert_eq!(rebuilt.message_ids(), actual_ids);
}

#[test]
fn versioned_input_journal_old_id_without_revision_stays_unknown() {
    let output = journal_output(
        json!({"text":"old words 👋\n","ids":["old-transfer"],"entry_id":"old-address"}),
    );
    let decoded = read_harvested_from_journal(&output).unwrap();
    let message = &decoded[0].messages[0];
    assert!(message.entry_ref.is_none());
    let rebuilt = QueueEntry::user_message_moved(
        message.text.clone(),
        message.ids.clone(),
        message.entry_ref.clone(),
    );
    assert_eq!(rebuilt.id(), None);
    assert_eq!(
        rebuilt.observation(),
        Observation::UserMessage {
            text: "old words 👋\n".into()
        }
    );
    assert_eq!(rebuilt.message_ids(), ["old-transfer"]);
    let segments = input_segments_for_entries(&CardId::from("journal-card"), &[rebuilt]);
    assert!(segments[0].origin.as_ref().unwrap().queue_entry.is_none());
    let unknown =
        journal_output(json!({"text":"legacy","ids":["id"],"entry_id":null,"entry_rev":null}));
    assert!(
        read_harvested_from_journal(&unknown).unwrap()[0].messages[0]
            .entry_ref
            .is_none()
    );
}

#[test]
fn versioned_input_journal_rejects_malformed_present_proof_without_changing_input() {
    for proof in [
        json!({"entry_id":"entry","entry_rev":-1}),
        json!({"entry_id":"entry","entry_rev":"1"}),
        json!({"entry_id":"entry","entry_rev":4294967296_u64}),
        json!({"entry_id":"entry","entry_rev":1.5}),
        json!({"entry_id":"entry","entry_rev":null}),
        json!({"entry_id":"","entry_rev":1}),
        json!({"entry_rev":1}),
        json!({"entry_id":null,"entry_rev":1}),
        json!({"entry_id":7,"entry_rev":1}),
    ] {
        let mut message = proof;
        message["text"] = json!("retain every word");
        message["ids"] = json!(["retain-transfer"]);
        let output = journal_output(message);
        let before = output.data.clone();
        assert!(
            matches!(
                read_harvested_from_journal(&output),
                Err(CalmError::Conflict(_))
            ),
            "{before}"
        );
        assert_eq!(output.data, before);
    }
}
