use super::{
    protocol::StopReason,
    translate::{TurnContext, TurnTranslator},
    usage::{UsageSource, UsageTracker},
};
use crate::events::{ItemPhase, PlannerEventKind};
use serde_json::{Value, json};

pub(super) fn translator() -> TurnTranslator {
    TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "session".into(),
        usage: UsageTracker::new(UsageSource::SessionUsage),
        background_tool_completions: false,
    })
}
pub(super) fn chunk(t: &mut TurnTranslator, content: Value) -> Vec<crate::events::PlannerEvent> {
    t.update(&json!({"sessionId":"session", "update":{"sessionUpdate":"agent_message_chunk", "messageId":"M", "content":content}}), 1).unwrap()
}
pub(super) fn completed(events: &[crate::events::PlannerEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            PlannerEventKind::Item {
                phase: ItemPhase::Completed,
                params,
                ..
            } => Some(params["item"].clone()),
            _ => None,
        })
        .collect()
}
#[test]
fn ordered_output_retains_resource_between_text() {
    let mut t = translator();
    chunk(&mut t, json!({"type":"text","text":"before"}));
    chunk(
        &mut t,
        json!({"type":"resource_link","uri":"https://example.test/item","name":"reference"}),
    );
    chunk(&mut t, json!({"type":"text","text":"after"}));
    let items = completed(&t.finish(StopReason::EndTurn, 2));
    let blocks = items[0]["outputContent"]["blocks"]
        .as_array()
        .expect("typed ordered content");
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[1]["type"], "resource_link");
    assert_eq!(blocks[2]["text"], "after");
}
#[test]
fn ordered_output_message_affiliation_does_not_reopen_completed_segment() {
    let mut t = translator();
    chunk(&mut t, json!({"type":"text","text":"before"}));
    let tool = t.update(&json!({"sessionId":"session", "update":{"sessionUpdate":"tool_call", "toolCallId":"tool", "title":"Read"}}), 2).unwrap();
    let first = completed(&tool);
    chunk(&mut t, json!({"type":"text","text":"after"}));
    let last = completed(&t.finish(StopReason::EndTurn, 3));
    assert_ne!(first[0]["id"], last[0]["id"]);
    assert_eq!(last[0]["text"], "after");
}
