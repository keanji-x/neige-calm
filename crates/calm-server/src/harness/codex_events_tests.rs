//! The Codex mapping, driven from the app-server's wire `(method, params)` through the production
//! parse: one test per mapped method, plus approval and unmodelled frames.

use serde_json::{Value, json};

use super::codex_events::planner_event;
use super::planner_event::{ItemPhase, PlannerEvent, PlannerEventKind};
use crate::codex_appserver::Notification;

fn wire(method: &str, params: Value) -> PlannerEvent {
    planner_event(Notification::parse(method.into(), params))
}

#[test]
fn thread_started_names_the_thread_it_started() {
    let event = wire("thread/started", json!({ "thread": { "id": "thr-1" } }));
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    assert!(matches!(event.kind, PlannerEventKind::ThreadStarted));
    let flat = wire("thread/started", json!({ "threadId": "thr-2" }));
    assert_eq!(flat.thread_id.as_deref(), Some("thr-2"));
    let unnamed = wire("thread/started", json!({}));
    assert_eq!(unnamed.thread_id, None);
    assert!(matches!(unnamed.kind, PlannerEventKind::ThreadStarted));
}

#[test]
fn thread_status_maps_system_error_and_idle_and_ignores_the_rest() {
    let status = |kind: &str| {
        wire(
            "thread/status/changed",
            json!({ "threadId": "thr-1", "status": { "type": kind } }),
        )
    };
    let failed = status("systemError");
    assert_eq!(failed.thread_id.as_deref(), Some("thr-1"));
    assert!(matches!(failed.kind, PlannerEventKind::ThreadSystemError));
    assert!(matches!(status("idle").kind, PlannerEventKind::ThreadIdle));
    let active = status("active");
    assert_eq!(active.thread_id.as_deref(), Some("thr-1"));
    assert!(matches!(active.kind, PlannerEventKind::Ignored));
}

#[test]
fn turn_started_carries_its_turn_id_and_one_without_an_id_is_ignored() {
    let event = wire(
        "turn/started",
        json!({ "threadId": "thr-1", "turn": { "id": "turn-1", "status": "inProgress" } }),
    );
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::TurnStarted { turn_id } = event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(turn_id, "turn-1");
    let no_id = wire("turn/started", json!({ "threadId": "thr-1", "turn": {} }));
    assert!(matches!(no_id.kind, PlannerEventKind::Ignored));
    // A frame without `threadId` names the empty thread, which no harness runs.
    let no_thread = wire("turn/started", json!({ "turn": { "id": "turn-1" } }));
    assert_eq!(no_thread.thread_id.as_deref(), Some(""));
}

#[test]
fn turn_completed_keeps_the_turn_record_whole() {
    let turn =
        json!({ "id": "turn-1", "status": "failed", "error": { "message": "boom" }, "items": [] });
    let event = wire(
        "turn/completed",
        json!({ "threadId": "thr-1", "turn": turn.clone() }),
    );
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::TurnCompleted { turn: carried } = event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(carried, turn);
}

#[test]
fn turn_aborted_reads_either_turn_id_shape_and_one_without_an_id_is_ignored() {
    for params in [
        json!({ "threadId": "thr-1", "turn": { "id": "turn-1" } }),
        json!({ "threadId": "thr-1", "turnId": "turn-1" }),
    ] {
        let event = wire("turn/aborted", params);
        assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
        let PlannerEventKind::TurnAborted { turn_id } = event.kind else {
            panic!("{event:?}");
        };
        assert_eq!(turn_id, "turn-1");
    }
    let no_id = wire("turn/aborted", json!({ "threadId": "thr-1" }));
    assert!(matches!(no_id.kind, PlannerEventKind::Ignored));
}

#[test]
fn item_started_and_completed_keep_their_params_and_other_item_frames_are_ignored() {
    let params = json!({
        "threadId": "thr-1", "turnId": "turn-1",
        "item": { "id": "it-1", "type": "agentMessage", "text": "hi" },
    });
    for (method, expected) in [
        ("item/started", ItemPhase::Started),
        ("item/completed", ItemPhase::Completed),
    ] {
        let event = wire(method, params.clone());
        assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
        let PlannerEventKind::Item {
            phase,
            params: carried,
        } = event.kind
        else {
            panic!("{method}: {event:?}");
        };
        assert_eq!((phase, phase.method()), (expected, method));
        assert_eq!(carried, params);
    }
    for method in ["item/reasoning/delta", "item/other"] {
        let event = wire(method, params.clone());
        assert_eq!(event.thread_id.as_deref(), Some("thr-1"), "{method}");
        assert!(matches!(event.kind, PlannerEventKind::Ignored), "{method}");
    }
}

#[test]
fn an_agent_message_delta_is_a_reply_delta_and_one_missing_a_field_is_ignored() {
    let delta =
        json!({ "threadId": "thr-1", "turnId": "turn-1", "itemId": "it-1", "delta": "Hel" });
    let event = wire("item/agentMessage/delta", delta.clone());
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::ReplyDelta {
        turn_id,
        item_id,
        delta: text,
    } = event.kind
    else {
        panic!("{event:?}");
    };
    assert_eq!(
        (turn_id.as_str(), item_id.as_str(), text.as_str()),
        ("turn-1", "it-1", "Hel")
    );
    for missing in ["turnId", "itemId", "delta"] {
        let mut partial = delta.clone();
        partial.as_object_mut().unwrap().remove(missing);
        let event = wire("item/agentMessage/delta", partial);
        assert!(matches!(event.kind, PlannerEventKind::Ignored), "{missing}");
    }
}

#[test]
fn plan_and_token_usage_keep_their_params() {
    let plan = json!({ "threadId": "thr-1", "turnId": "turn-1", "plan": [{ "step": "a", "status": "pending" }] });
    let event = wire("turn/plan/updated", plan.clone());
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::PlanUpdated { params } = event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(params, plan);

    let usage = json!({ "threadId": "thr-1", "tokenUsage": { "last": { "totalTokens": 5 } } });
    let event = wire("thread/tokenUsage/updated", usage.clone());
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::TokenUsage { params } = event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(params, usage);
}

#[test]
fn an_approval_method_maps_to_approval() {
    let event = wire("approval/commandExecution", json!({ "threadId": "thr-1" }));
    assert_eq!(event.thread_id.as_deref(), Some("thr-1"));
    let PlannerEventKind::Approval { method } = event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(method, "approval/commandExecution");
}

#[test]
fn an_unmodelled_method_is_ignored_on_its_thread() {
    for method in [
        "thread/realtime/sdp",
        "turn/diff/updated",
        "account/updated",
    ] {
        let event = wire(method, json!({ "threadId": "thr-1" }));
        assert_eq!(event.thread_id.as_deref(), Some("thr-1"), "{method}");
        assert!(matches!(event.kind, PlannerEventKind::Ignored), "{method}");
    }
    let unthreaded = wire("account/updated", json!({}));
    assert_eq!(unthreaded.thread_id, None);
    assert!(matches!(unthreaded.kind, PlannerEventKind::Ignored));
}
