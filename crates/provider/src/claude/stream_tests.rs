//! A Claude reply's text while it streams (#1923 S2 P3): the `stream_event` frames of
//! `--include-partial-messages`, decoded and translated. `p1923_stream` is described in `tests`.

use serde_json::{Value, json};

use super::protocol::{BlockKind, ProtocolError, Record, StreamEvent, decode};
use super::tests::{context, decode_fixture, fixture_lines, items, visible_tools};
use super::translate::TurnTranslator;
use crate::events::{PlannerEvent, PlannerEventKind};

const STREAM: &str = "p1923_stream.ndjson";
const FIRST: &str = "msg_REDACTED0027";
const SECOND: &str = "msg_REDACTED0028";
const TOOL: &str = "toolu_01F1VAfHM25psKYSFr8qDMN9";

fn translator() -> TurnTranslator {
    TurnTranslator::new(context("0".repeat(32), Vec::new()), visible_tools()).unwrap()
}

/// Every record through one translator, one millisecond apart from 1000.
fn translate(records: &[Record]) -> Vec<PlannerEvent> {
    let mut translator = translator();
    (0_i64..)
        .zip(records)
        .flat_map(|(ms, record)| translator.translate(record, 1_000 + ms))
        .collect()
}

/// Each item event as `(method, id, type)`, in order.
fn item_events(events: &[PlannerEvent]) -> Vec<(String, String, String)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            PlannerEventKind::Item {
                phase,
                params,
                questions,
            } => {
                assert!(
                    questions.is_empty(),
                    "a Claude item never asks: {questions:?}"
                );
                Some((
                    phase.method().to_owned(),
                    params["item"]["id"].as_str().unwrap().to_owned(),
                    params["item"]["type"].as_str().unwrap().to_owned(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// Each reply delta as `(item id, delta)`, in order; every one names the issued turn.
fn deltas(events: &[PlannerEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            PlannerEventKind::ReplyDelta {
                turn_id,
                item_id,
                delta,
            } => {
                assert_eq!(turn_id, "turn-1");
                assert_eq!(event.thread_id.as_deref(), Some("thread-1"));
                Some((item_id.clone(), delta.clone()))
            }
            _ => None,
        })
        .collect()
}

fn streamed(item_id: &str, deltas: &[(String, String)]) -> String {
    deltas
        .iter()
        .filter(|(id, _)| id == item_id)
        .map(|(_, delta)| delta.as_str())
        .collect()
}

fn event(method: &str, id: &str, item_type: &str) -> (String, String, String) {
    (method.into(), id.into(), item_type.into())
}

#[test]
fn a_streamed_turn_starts_each_reply_at_its_block_and_completes_it_by_its_record() {
    let events = translate(&decode_fixture(STREAM));
    let (first_text, second_text) = (format!("{FIRST}:1"), format!("{SECOND}:1"));
    assert_eq!(
        item_events(&events),
        [
            event("item/started", &format!("{FIRST}:0"), "reasoning"),
            event("item/completed", &format!("{FIRST}:0"), "reasoning"),
            event("item/started", &first_text, "agentMessage"),
            event("item/completed", &first_text, "agentMessage"),
            event("item/started", TOOL, "dynamicToolCall"),
            event("item/completed", TOOL, "dynamicToolCall"),
            event("item/started", &format!("{SECOND}:0"), "reasoning"),
            event("item/completed", &format!("{SECOND}:0"), "reasoning"),
            event("item/started", &second_text, "agentMessage"),
            event("item/completed", &second_text, "agentMessage"),
        ],
        "one start per item, stream ids equal record ids, the tool keeps its tool_use id"
    );
    let completed = items(&events, "item/completed");
    let texts: Vec<&Value> = completed
        .iter()
        .filter(|item| item["type"] == "agentMessage")
        .map(|item| &item["text"])
        .collect();
    assert_eq!(
        texts,
        [
            &json!("I'll read the file to find the secret word."),
            &json!("The secret word is marmalade."),
        ]
    );
    let deltas = deltas(&events);
    assert_eq!(streamed(&first_text, &deltas), texts[0].as_str().unwrap());
    assert_eq!(streamed(&second_text, &deltas), texts[1].as_str().unwrap());
    let text_delta_lines = fixture_lines(STREAM)
        .iter()
        .filter(|line| line.contains(r#""type":"text_delta""#))
        .count();
    assert_eq!(
        deltas.len(),
        text_delta_lines,
        "one ReplyDelta per text_delta"
    );
    let started = items(&events, "item/started");
    let reply_start = started
        .iter()
        .find(|item| item["id"] == first_text.as_str())
        .unwrap();
    assert_eq!(
        **reply_start,
        json!({ "id": first_text, "type": "agentMessage", "text": "" })
    );
}

/// The wire this rests on: the CLI writes each block's record after the block starts and before
/// its `content_block_stop`, one record per block.
#[test]
fn every_captured_block_gets_its_record_before_it_stops() {
    let mut awaiting: Option<u64> = None;
    let mut records = 0;
    for line in fixture_lines(STREAM) {
        let value: Value = serde_json::from_str(&line).unwrap();
        let event = &value["event"];
        match (value["type"].as_str(), event["type"].as_str()) {
            (Some("stream_event"), Some("content_block_start")) => {
                assert_eq!(awaiting, None, "{line}");
                awaiting = event["index"].as_u64();
            }
            (Some("assistant"), _) => {
                assert!(
                    awaiting.take().is_some(),
                    "a record with no open block: {line}"
                );
                assert_eq!(value["message"]["content"].as_array().unwrap().len(), 1);
                records += 1;
            }
            (Some("stream_event"), Some("content_block_stop")) => {
                assert_eq!(awaiting, None, "a block stopped before its record: {line}");
            }
            _ => {}
        }
    }
    assert_eq!(records, 5);
}

/// A reply's start comes before its first delta, and its last delta before its completion.
#[test]
fn a_replys_deltas_fall_between_its_start_and_its_completion() {
    let events = translate(&decode_fixture(STREAM));
    for id in [format!("{FIRST}:1"), format!("{SECOND}:1")] {
        let positions: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.kind {
                PlannerEventKind::Item { phase, params, .. }
                    if params["item"]["id"] == id.as_str() =>
                {
                    Some(phase.method())
                }
                PlannerEventKind::ReplyDelta { item_id, .. } if *item_id == id => Some("delta"),
                _ => None,
            })
            .collect();
        let (first, rest) = positions.split_first().unwrap();
        let (last, middle) = rest.split_last().unwrap();
        assert_eq!(
            (*first, *last),
            ("item/started", "item/completed"),
            "{id}: {positions:?}"
        );
        assert!(
            !middle.is_empty() && middle.iter().all(|p| *p == "delta"),
            "{id}: {positions:?}"
        );
    }
}

/// The stream changes when a reply starts and the ids it gets, and nothing a stored row keeps:
/// the same records without their `stream_event` lines (the wire before #1923) give the same
/// items, under the same ids, which on this wire equal a block's place among its message's records.
#[test]
fn the_stream_changes_no_stored_item() {
    let records = decode_fixture(STREAM);
    let without_stream: Vec<Record> = records
        .iter()
        .filter(|record| !matches!(record, Record::Stream(_)))
        .cloned()
        .collect();
    let stored = |events: &[PlannerEvent]| -> Vec<(String, Value)> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                PlannerEventKind::Item { phase, params, .. } => {
                    Some((phase.method().to_owned(), params["item"].clone()))
                }
                _ => None,
            })
            .collect()
    };
    let streamed = translate(&records);
    assert_eq!(
        stored(&streamed),
        stored(&translate(&without_stream)),
        "only start times differ"
    );
}

/// The interrupted turn's text block had deltas and no record: its start stays open, and closing
/// the turn's tools does not complete it (the run loop's settle hook stores it as a partial).
#[test]
fn a_text_block_cut_off_before_its_record_stays_open() {
    let records = decode_fixture(STREAM);
    let cut = records
        .iter()
        .position(|record| {
            matches!(record, Record::Stream(StreamEvent::TextDelta { text, .. }) if text == "The secret word is mar")
        })
        .unwrap();
    let mut translator = translator();
    let events: Vec<PlannerEvent> = records[..=cut]
        .iter()
        .flat_map(|record| translator.translate(record, 1))
        .collect();
    let reply = format!("{SECOND}:1");
    assert_eq!(
        item_events(&events).last(),
        Some(&event("item/started", &reply, "agentMessage"))
    );
    assert_eq!(
        deltas(&events).last(),
        Some(&(reply.clone(), "The secret word is mar".to_owned()))
    );
    assert!(translator.close_open(2).is_empty());
}

fn stream_line(event: Value) -> String {
    json!({
        "type": "stream_event", "event": event, "session_id": "s",
        "parent_tool_use_id": null, "uuid": "6ca4287e-8ed8-4b41-a543-25ddaf55de4e",
    })
    .to_string()
}

fn text_record(message_id: &str, text: &str) -> String {
    json!({
        "type": "assistant", "uuid": "6ca4287e-8ed8-4b41-a543-25ddaf55de4f", "session_id": "s",
        "message": { "id": message_id, "content": [{ "type": "text", "text": text }] },
    })
    .to_string()
}

/// The record takes the index of the stream block it closes, even where that is not its place
/// among the message's records: block 0 here never gets a record.
#[test]
fn a_record_takes_its_stream_blocks_index() {
    let lines = [
        stream_line(json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "before any message" } })),
        stream_line(json!({ "type": "message_start", "message": { "id": "msg_1" } })),
        stream_line(json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "redacted_thinking", "data": "x" } })),
        stream_line(json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "text", "text": "" } })),
        stream_line(json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "another block" } })),
        stream_line(json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "hi" } })),
        text_record("msg_1", "hi"),
    ];
    let records: Vec<Record> = lines.iter().map(|line| decode(line).unwrap()).collect();
    let events = translate(&records);
    assert_eq!(
        item_events(&events),
        [
            event("item/started", "msg_1:1", "agentMessage"),
            event("item/completed", "msg_1:1", "agentMessage"),
        ]
    );
    assert_eq!(deltas(&events), [("msg_1:1".to_owned(), "hi".to_owned())]);
}

/// A nested stream's frames (a non-null `parent_tool_use_id`) are ignored: they neither open a
/// reply nor move the top-level message's open block.
#[test]
fn a_nested_streams_frames_are_ignored() {
    let nested = |event: Value| {
        let mut line: Value = serde_json::from_str(&stream_line(event)).unwrap();
        line["parent_tool_use_id"] = json!("toolu_parent");
        line.to_string()
    };
    let lines = [
        stream_line(json!({ "type": "message_start", "message": { "id": "msg_1" } })),
        stream_line(json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" } })),
        nested(json!({ "type": "message_start", "message": { "id": "msg_sub" } })),
        nested(json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "" } })),
        nested(json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "nested" } })),
        stream_line(json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "hi" } })),
        text_record("msg_1", "hi"),
    ];
    let records: Vec<Record> = lines.iter().map(|line| decode(line).unwrap()).collect();
    for record in &records[2..5] {
        assert_eq!(
            *record,
            Record::Ignored {
                kind: "stream_event/nested".into()
            }
        );
    }
    let events = translate(&records);
    assert_eq!(
        item_events(&events),
        [
            event("item/started", "msg_1:0", "agentMessage"),
            event("item/completed", "msg_1:0", "agentMessage"),
        ]
    );
    assert_eq!(deltas(&events), [("msg_1:0".to_owned(), "hi".to_owned())]);
}

#[test]
fn stream_events_decode_only_what_a_live_reply_needs() {
    let decoded = |event: Value| decode(&stream_line(event));
    assert_eq!(
        decoded(json!({ "type": "message_start", "message": { "id": "msg_1", "content": [] } }))
            .unwrap(),
        Record::Stream(StreamEvent::MessageStart {
            message_id: "msg_1".into()
        })
    );
    for (wire, kind) in [
        ("text", BlockKind::Text),
        ("thinking", BlockKind::Thinking),
        ("tool_use", BlockKind::ToolUse),
        ("server_tool_use", BlockKind::Other),
    ] {
        assert_eq!(
            decoded(json!({ "type": "content_block_start", "index": 2,
                "content_block": { "type": wire } }))
            .unwrap(),
            Record::Stream(StreamEvent::BlockStart { index: 2, kind })
        );
    }
    assert_eq!(
        decoded(json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "Hel" } }))
        .unwrap(),
        Record::Stream(StreamEvent::TextDelta {
            index: 1,
            text: "Hel".into()
        })
    );
    for (event, kind) in [
        (
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "" } }),
            "stream_event/content_block_delta",
        ),
        (
            json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "{" } }),
            "stream_event/content_block_delta",
        ),
        (
            json!({ "type": "content_block_stop", "index": 1 }),
            "stream_event/content_block_stop",
        ),
        (
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }),
            "stream_event/message_delta",
        ),
        (
            json!({ "type": "message_stop" }),
            "stream_event/message_stop",
        ),
    ] {
        assert_eq!(
            decoded(event).unwrap(),
            Record::Ignored { kind: kind.into() }
        );
    }
    for broken in [
        json!({ "type": "message_start", "message": {} }),
        json!({ "type": "content_block_start", "content_block": { "type": "text" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta" } }),
    ] {
        assert!(
            matches!(decoded(broken.clone()), Err(ProtocolError::Shape { .. })),
            "{broken}"
        );
    }
    assert!(matches!(
        decode(r#"{"type":"stream_event","event":{"index":1}}"#),
        Err(ProtocolError::NoSubtype { .. })
    ));
    assert!(matches!(
        decode(r#"{"type":"assistant","message":{"content":[]}}"#),
        Err(ProtocolError::Shape { .. })
    ));
}
