use super::{
    translate::{TurnContext, TurnTranslator},
    usage::{UsageSource, UsageTracker},
};
use crate::events::PlannerEventKind;
use serde_json::{Value, json};

fn tracker() -> UsageTracker {
    let mut tracker = UsageTracker::new(UsageSource::XaiNotificationMeta);
    tracker.configure(&json!({"models":{"currentModelId":"a", "availableModels":[
        {"modelId":"a", "_meta":{"totalContextTokens":200000}},
        {"modelId":"b", "_meta":{"totalContextTokens":500000}}
    ]}}));
    tracker
}
fn frame(used: Value) -> Value {
    json!({"sessionId":"native", "update":{"sessionUpdate":"agent_message_chunk",
        "content":{"type":"text","text":"reply"}}, "_meta":{"totalTokens":used}})
}

#[test]
fn xai_notification_context_tracks_model_selection_and_occupancy_decreases() {
    let mut tracker = tracker();
    assert_eq!(tracker.reading(&frame(json!(60200))), Some((60200, 200000)));
    assert_eq!(tracker.reading(&frame(json!(24100))), Some((24100, 200000)));
    tracker.configure(&json!({"configOptions":[{"category":"model","currentValue":"b"}]}));
    assert_eq!(tracker.reading(&frame(json!(24100))), Some((24100, 500000)));
    let mut changed = frame(json!(1000));
    changed["update"] = json!({"sessionUpdate":"current_model_update","currentModelId":"a"});
    assert_eq!(tracker.reading(&changed), Some((1000, 200000)));
    changed["update"]["currentModelId"] = json!("unknown");
    assert_eq!(tracker.reading(&changed), None);
}

#[test]
fn xai_usage_does_not_fabricate_windows_or_accept_replay_and_invalid_counts() {
    assert_eq!(
        UsageTracker::new(UsageSource::XaiNotificationMeta).reading(&frame(json!(24100))),
        None
    );
    let mut tracker = tracker();
    for used in [
        json!(-1),
        json!(null),
        json!(1.5),
        json!("24100"),
        json!(18446744073709551615_u64),
    ] {
        assert_eq!(tracker.reading(&frame(used)), None);
    }
    let mut replay = frame(json!(24100));
    replay["_meta"]["isReplay"] = json!(true);
    assert_eq!(tracker.reading(&replay), None);
    assert_eq!(tracker.reading(&frame(json!(0))), Some((0, 200000)));
    assert_eq!(
        tracker.reading(&frame(json!(210000))),
        Some((210000, 200000))
    );
    let mut standard = UsageTracker::new(UsageSource::SessionUsage);
    assert_eq!(standard.reading(&frame(json!(24100))), None);
}

#[test]
fn xai_notification_uses_current_neige_turn_and_preserves_the_reply() {
    let mut translator = TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "native".into(),
        usage: tracker(),
    });
    let events = translator.update(&frame(json!(24100)), 10).unwrap();
    let usage = events
        .iter()
        .find_map(|event| match &event.kind {
            PlannerEventKind::TokenUsage { params } => Some(params),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage["turnId"], "turn");
    assert_eq!(usage["tokenUsage"]["last"]["totalTokens"], 24100);
    assert_eq!(usage["tokenUsage"]["modelContextWindow"], 200000);
    assert!(usage["tokenUsage"].get("total").is_none());
    assert!(events.iter().any(
        |event| matches!(&event.kind, PlannerEventKind::ReplyDelta {delta,..} if delta=="reply")
    ));
    let mut foreign = frame(json!(24100));
    foreign["sessionId"] = json!("other");
    assert!(translator.update(&foreign, 10).is_err());
}
