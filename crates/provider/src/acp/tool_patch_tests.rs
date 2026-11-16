use super::{
    translate::{TurnContext, TurnTranslator},
    usage::{UsageSource, UsageTracker},
};
use crate::events::{ItemPhase, PlannerEventKind};
use serde_json::{Value, json};

fn translator() -> TurnTranslator {
    TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "session".into(),
        usage: UsageTracker::new(UsageSource::SessionUsage),
        background_tool_completions: false,
    })
}

fn frame(update: Value) -> Value {
    json!({"sessionId":"session", "update":update})
}

fn item(translator: &mut TurnTranslator, update: Value) -> Value {
    let events = translator.update(&frame(update), 1).unwrap();
    let [event] = events.as_slice() else {
        panic!("expected one tool item")
    };
    let PlannerEventKind::Item { params, .. } = &event.kind else {
        panic!("expected tool item")
    };
    params["item"].clone()
}

#[test]
fn null_status_patch_preserves_running_tool() {
    let mut translator = translator();
    item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a",
        "title":"Read", "status":"in_progress"}),
    );
    let updated = item(
        &mut translator,
        json!({"sessionUpdate":"tool_call_update",
        "toolCallId":"a", "status":null}),
    );
    assert_eq!(updated["status"], "inProgress");
    assert_eq!(updated["native"]["status"], "in_progress");
}

// Exercise the production merge for each field, including collection replacement.
fn field_patches(field: &str, values: Vec<Value>) {
    let mut translator = translator();
    item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"}),
    );
    for value in values {
        let mut patch = json!({"sessionUpdate":"tool_call_update", "toolCallId":"a"});
        patch[field] = value.clone();
        let changed = item(&mut translator, patch);
        assert_eq!(changed["native"][field], value, "replace {field}");
        for patch in [
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a"}),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", (field):null}),
        ] {
            let retained = item(&mut translator, patch);
            assert_eq!(retained["native"][field], value, "retain {field}");
            match field {
                "title" => assert_eq!(retained["tool"], value),
                "rawInput" => assert_eq!(retained["arguments"], value),
                "rawOutput" => assert_eq!(retained["result"]["structuredContent"], value),
                "content" => assert_eq!(
                    retained["result"]["content"],
                    Value::Array(
                        value
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|part| part["content"].clone())
                            .collect()
                    )
                ),
                "status" => assert_eq!(
                    retained["status"],
                    match value.as_str().unwrap() {
                        "pending" | "in_progress" => "inProgress",
                        status => status,
                    }
                ),
                _ => {}
            }
        }
    }
}

#[test]
fn title_patches_preserve_or_replace_including_empty_string() {
    field_patches("title", vec![json!("Read"), json!("Search"), json!("")]);
}

#[test]
fn kind_patches_preserve_or_replace() {
    field_patches("kind", vec![json!("read"), json!("search"), json!("other")]);
}

#[test]
fn status_patches_preserve_or_replace() {
    field_patches(
        "status",
        vec![
            json!("pending"),
            json!("in_progress"),
            json!("failed"),
            json!("completed"),
        ],
    );
}

#[test]
fn content_patches_preserve_or_replace_including_empty_collection() {
    field_patches(
        "content",
        vec![
            json!([{"type":"content", "content":{"type":"text", "text":"first"}}]),
            json!([{"type":"content", "content":{"type":"text", "text":""}}]),
            json!([]),
        ],
    );
}

#[test]
fn locations_patches_preserve_or_replace_including_empty_collection() {
    field_patches(
        "locations",
        vec![
            json!([{"path":"/a", "line":1}]),
            json!([{"path":"/b"}]),
            json!([]),
        ],
    );
}

#[test]
fn raw_input_patches_preserve_or_replace_all_non_null_json_values() {
    field_patches(
        "rawInput",
        vec![
            json!({"path":"/a"}),
            json!({}),
            json!([1]),
            json!([]),
            json!("text"),
            json!(""),
            json!(0),
            json!(false),
        ],
    );
}

#[test]
fn raw_output_patches_preserve_or_replace_all_non_null_json_values() {
    field_patches(
        "rawOutput",
        vec![
            json!({"output":"ok"}),
            json!({}),
            json!([1]),
            json!([]),
            json!("text"),
            json!(""),
            json!(0),
            json!(false),
        ],
    );
}

#[test]
fn initial_raw_values_omitted_or_null_mean_no_value() {
    for raw in [json!({}), json!({"rawInput":null, "rawOutput":null})] {
        let mut translator = translator();
        let mut call = json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"});
        call.as_object_mut()
            .unwrap()
            .extend(raw.as_object().unwrap().clone());
        let initial = item(&mut translator, call);
        assert!(initial["arguments"].is_null());
        assert!(initial["result"]["structuredContent"].is_null());
        for field in ["rawInput", "rawOutput"] {
            assert!(initial["native"].get(field).is_none());
        }
    }
}

#[test]
fn mixed_patches_keep_interleaved_tools_and_completed_output_independent() {
    let mut translator = translator();
    for id in ["a", "b"] {
        item(
            &mut translator,
            json!({"sessionUpdate":"tool_call", "toolCallId":id,
            "title":id, "name":id, "kind":"read", "status":"in_progress",
            "rawInput":{"path":id}, "rawOutput":id,
            "content":[{"type":"content", "content":{"type":"text", "text":id}}],
            "locations":[{"path":format!("/{id}")}]}),
        );
    }
    let a = item(
        &mut translator,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"a",
        "title":"", "kind":null, "name":null, "status":null, "rawInput":false,
        "rawOutput":null, "content":[], "locations":null}),
    );
    assert_eq!(a["native"]["name"], "a");
    assert_eq!(a["native"]["kind"], "read");
    assert_eq!(a["tool"], "");
    assert_eq!(a["arguments"], false);
    assert_eq!(a["result"]["content"], json!([]));
    let b = item(
        &mut translator,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"b",
        "title":null, "kind":"search", "status":"completed", "rawInput":null,
        "rawOutput":0, "content":null, "locations":[]}),
    );
    assert_eq!(b["native"]["name"], "b");
    assert_eq!(b["native"]["kind"], "search");
    assert_eq!(b["native"]["locations"], json!([]));
    assert_eq!(b["tool"], "b");
    assert_eq!(b["arguments"], json!({"path":"b"}));
    assert_eq!(b["result"]["structuredContent"], 0);
    assert_eq!(b["result"]["content"], json!([{"type":"text", "text":"b"}]));
    let events = translator
        .update(
            &frame(json!({"sessionUpdate":"tool_call_update", "toolCallId":"a",
        "status":"completed", "title":null, "content":null, "rawInput":null, "rawOutput":null})),
            2,
        )
        .unwrap();
    let PlannerEventKind::Item { params, phase, .. } = &events[0].kind else {
        panic!("expected tool item")
    };
    assert_eq!(*phase, ItemPhase::Completed);
    let completed = &params["item"];
    assert_eq!(completed["id"], "acp-turn-tool-a");
    assert_eq!(completed["native"]["status"], "completed");
    assert_eq!(completed["native"]["locations"], json!([{"path":"/a"}]));
    assert_eq!(completed["tool"], "");
    assert_eq!(completed["arguments"], false);
    assert_eq!(completed["result"]["structuredContent"], "a");
    assert_eq!(completed["result"]["content"], json!([]));
}

#[test]
fn opaque_meta_keeps_existing_merge_behavior() {
    let mut translator = translator();
    item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read", "_meta":{"custom":1}}),
    );
    for (patch, expected) in [
        (json!({}), json!({"custom":1})),
        (json!({"_meta":{"other":false}}), json!({"other":false})),
        (json!({"_meta":null}), Value::Null),
    ] {
        let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":"a"});
        update
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(item(&mut translator, update)["native"]["_meta"], expected);
    }
}

#[test]
fn null_patch_handling_does_not_relax_invalid_tool_updates() {
    for patch in [
        json!({"toolCallId":"a", "status":"unknown"}),
        json!({"toolCallId":null, "status":null}),
        json!({"status":null}),
        json!({"toolCallId":"missing", "status":null}),
    ] {
        let mut translator = translator();
        item(
            &mut translator,
            json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"}),
        );
        let mut update = patch;
        update["sessionUpdate"] = json!("tool_call_update");
        assert!(matches!(
            translator.update(&frame(update), 1),
            Err(super::Error::Protocol(_))
        ));
    }
    for title in [None, Some(Value::Null), Some(json!(0))] {
        let mut translator = translator();
        let mut call = json!({"sessionUpdate":"tool_call", "toolCallId":"a"});
        if let Some(title) = title {
            call["title"] = title;
        }
        assert!(matches!(
            translator.update(&frame(call), 1),
            Err(super::Error::Protocol(_))
        ));
    }
    let mut translator = translator();
    let call = json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"});
    item(&mut translator, call.clone());
    assert!(matches!(
        translator.update(&frame(call), 1),
        Err(super::Error::Protocol(_))
    ));
    let mut foreign =
        frame(json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "status":null}));
    foreign["sessionId"] = json!("foreign");
    assert!(matches!(
        translator.update(&foreign, 1),
        Err(super::Error::Protocol(_))
    ));
}

#[test]
fn background_terminal_updates_still_require_this_turn_to_own_tool() {
    let mut translator = TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "session".into(),
        usage: UsageTracker::new(UsageSource::SessionUsage),
        background_tool_completions: true,
    });
    for status in ["completed", "failed"] {
        let late = json!({"sessionUpdate":"tool_call_update", "toolCallId":"old", "status":status, "content":null});
        assert!(translator.update(&frame(late), 1).unwrap().is_empty());
    }
    assert!(matches!(
        translator.update(
            &frame(json!({"sessionUpdate":"tool_call_update", "toolCallId":"old", "status":null})),
            1
        ),
        Err(super::Error::Protocol(_))
    ));
    item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"}),
    );
    assert_eq!(
        item(
            &mut translator,
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "status":"completed", "content":null})
        )["status"],
        "completed"
    );
}
