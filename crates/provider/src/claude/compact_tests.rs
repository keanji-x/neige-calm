//! The wire of a manual compaction: the `/compact` line, the `system/compact_boundary` that answers
//! it, and the init's command list, which says whether the line can run.

use serde_json::json;
use uuid::Uuid;

use super::protocol::{COMPACT_COMMAND, Record, UserLine, chain_entry_uuid, decode};
use super::tests::{context, fixture_lines, visible_tools};
use super::translate::TurnTranslator;

#[test]
fn a_compact_boundary_declares_context_replacement_without_metadata() {
    let line = json!({
        "type": "system",
        "subtype": "compact_boundary",
        "session_id": Uuid::new_v4().to_string(),
        "uuid": Uuid::new_v4().to_string(),
        "compact_metadata": {"trigger": "manual", "pre_tokens": 1234},
    })
    .to_string();
    let record = decode(&line).unwrap();
    assert_eq!(record, Record::CompactBoundary);
    assert_eq!(
        chain_entry_uuid(&line),
        None,
        "a system line is no chain entry"
    );
    let mut translator =
        TurnTranslator::new(context("0".repeat(32), Vec::new()), visible_tools()).unwrap();
    let events = translator.translate_line(&line, &record, 1);
    assert_eq!(
        events.len(),
        1,
        "a declared boundary must reach the harness"
    );
    assert!(
        matches!(&events[0].kind, crate::events::PlannerEventKind::ContextReplaced { turn_id } if turn_id == "turn-1")
    );
    // The record alone says the session compacted; its metadata is unnecessary to that.
    assert_eq!(
        decode(r#"{"type":"system","subtype":"compact_boundary"}"#).unwrap(),
        Record::CompactBoundary
    );
    assert!(matches!(
        decode(r#"{"type":"system","subtype":"compacting"}"#).unwrap(),
        Record::Ignored { kind } if kind == "system/compacting"
    ));
}

#[test]
fn the_compact_line_is_the_slash_command_as_one_text_block() {
    let session = Uuid::new_v4();
    let uuid = Uuid::new_v4();
    assert_eq!(
        serde_json::to_value(UserLine::compact(session, uuid)).unwrap(),
        json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": "/compact"}]},
            "parent_tool_use_id": null,
            "session_id": session.to_string(),
            "uuid": uuid.to_string(),
        })
    );
}

/// Recorded on the pinned CLI: a spawn under `--disable-slash-commands` lists no command, so it
/// cannot run `/compact`; one without the flag lists the built-in `compact`.
#[test]
fn recorded_inits_list_compact_only_when_slash_commands_are_enabled() {
    let init = |name: &str| {
        fixture_lines(name)
            .iter()
            .find_map(|line| match decode(line).unwrap() {
                Record::SystemInit(init) => Some(init),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} has an init"))
    };
    assert!(init("pE.ndjson").slash_commands.is_empty());
    assert!(
        init("pB_baseline.ndjson")
            .slash_commands
            .iter()
            .any(|command| command == COMPACT_COMMAND)
    );
}
