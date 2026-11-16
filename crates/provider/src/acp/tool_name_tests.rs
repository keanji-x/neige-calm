use super::{
    translate::{TurnContext, TurnTranslator},
    usage::{UsageSource, UsageTracker},
};
use crate::events::PlannerEventKind;
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

fn item(translator: &mut TurnTranslator, update: Value) -> Value {
    let events = translator
        .update(&json!({"sessionId":"session", "update":update}), 1)
        .unwrap();
    let [event] = events.as_slice() else {
        panic!("expected one tool item");
    };
    let PlannerEventKind::Item { params, .. } = &event.kind else {
        panic!("expected tool item");
    };
    params["item"].clone()
}

#[test]
fn tool_name_survives_initial_call_and_status_update_as_native_metadata() {
    let mut translator = translator();
    let initial = item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a",
            "title":"Read configuration", "name":"read_file"}),
    );
    let completed = item(
        &mut translator,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "status":"completed"}),
    );
    assert_eq!(completed["native"]["name"], "read_file");
    assert_eq!(initial["native"]["name"], "read_file");
    for item in [initial, completed] {
        assert_eq!(item["type"], "dynamicToolCall");
        assert_eq!(item["tool"], "Read configuration");
        assert!(item.get("server").is_none());
    }
}

#[test]
fn tool_name_updates_replace_strings_and_preserve_on_omitted_or_null() {
    let mut translator = translator();
    item(
        &mut translator,
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "title":"Read"}),
    );
    // A name may first arrive in an update; even an empty string is opaque metadata.
    for name in ["read_file", "corrected_name", ""] {
        let changed = item(
            &mut translator,
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "name":name}),
        );
        assert_eq!(changed["native"]["name"], name);
        for patch in [
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a"}),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "name":null}),
        ] {
            let retained = item(&mut translator, patch);
            assert_eq!(retained["native"]["name"], name);
            assert_eq!(retained["tool"], "Read");
        }
    }
}

#[test]
fn tool_name_absent_or_null_keeps_ordinary_tools_compatible() {
    for name in [None, Some(Value::Null)] {
        let mut translator = translator();
        let mut call = json!({"sessionUpdate":"tool_call", "toolCallId":"plain",
            "title":"Read", "rawInput":{"path":"config"}});
        if let Some(name) = name {
            call["name"] = name;
        }
        let initial = item(&mut translator, call);
        assert_eq!(
            initial["native"],
            json!({"toolCallId":"plain", "status":"pending",
            "title":"Read", "rawInput":{"path":"config"}})
        );
        for patch in [
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"plain", "name":null}),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"plain", "status":"completed"}),
        ] {
            let updated = item(&mut translator, patch);
            assert!(updated["native"].get("name").is_none());
            assert_eq!(updated["type"], "dynamicToolCall");
            assert_eq!(updated["tool"], "Read");
            assert_eq!(updated["arguments"], json!({"path":"config"}));
        }
    }
}

#[test]
fn tool_name_interleaved_calls_keep_metadata_scoped_to_tool_call_id() {
    let mut translator = translator();
    for (id, name) in [("a", "read_file"), ("b", "search_files")] {
        item(
            &mut translator,
            json!({"sessionUpdate":"tool_call", "toolCallId":id,
                "title":"Same display title", "name":name}),
        );
    }
    for (id, patch, expected) in [
        ("a", json!({"name":"corrected_read"}), "corrected_read"),
        ("b", json!({"name":null}), "search_files"),
        ("a", json!({}), "corrected_read"),
        ("b", json!({"name":"corrected_search"}), "corrected_search"),
        ("a", json!({"name":null}), "corrected_read"),
    ] {
        let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":id});
        update
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let updated = item(&mut translator, update);
        assert_eq!(updated["id"], format!("acp-turn-tool-{id}"));
        assert_eq!(updated["native"]["toolCallId"], id);
        assert_eq!(updated["native"]["name"], expected);
    }
}
