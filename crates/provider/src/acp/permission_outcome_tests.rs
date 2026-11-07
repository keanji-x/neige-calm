use super::{
    approvals::DeclinedPermission,
    protocol::StopReason,
    translate::{TurnContext, TurnTranslator},
};
use crate::events::PlannerEventKind;
use serde_json::{Value, json};

fn outcome(frames: Vec<Value>, reason: StopReason, tool: &str) -> Value {
    let mut translator = TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "session".into(),
        usage: super::usage::UsageTracker::new(super::usage::UsageSource::SessionUsage),
    });
    for frame in frames {
        translator
            .update(&json!({"sessionId":"session", "update":frame}), 10)
            .unwrap();
    }
    let denied = DeclinedPermission {
        tool_call_id: tool.into(),
        reason: "untrusted native title".into(),
        diagnostic: super::diagnostics::AcpDiagnostic::Never,
    };
    let events = translator.finish_after_permission(reason, Some(&denied), 20);
    let PlannerEventKind::TurnCompleted { turn } = &events.last().unwrap().kind else {
        panic!("outcome")
    };
    turn.clone()
}
fn tool(id: &str, status: &str) -> Value {
    json!({"sessionUpdate":"tool_call", "toolCallId":id, "title":id, "status":status})
}
fn reply(text: &str) -> Value {
    json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":text}})
}

#[test]
fn a_correlated_denial_explains_silent_end_after_success_and_collateral_failures() {
    let turn = outcome(
        vec![
            tool("read", "completed"),
            tool("denied", "failed"),
            tool("cancelled-peer", "failed"),
        ],
        StopReason::EndTurn,
        "denied",
    );
    assert_eq!(turn["status"], "failed");
    assert_eq!(turn["error"]["code"], "acp_permission_denied");
}

#[test]
fn denial_does_not_override_recovery_or_a_user_interruption() {
    for frames in [
        vec![tool("denied", "failed"), tool("fallback", "completed")],
        vec![tool("denied", "failed"), reply("The tool was denied.")],
    ] {
        assert_eq!(
            outcome(frames, StopReason::EndTurn, "denied")["status"],
            "completed"
        );
    }
    let turn = outcome(
        vec![tool("denied", "failed")],
        StopReason::Cancelled,
        "denied",
    );
    assert_eq!(turn["status"], "interrupted");
    assert!(turn["error"].is_null());
}

#[test]
fn a_stale_decline_does_not_explain_an_unrelated_failure() {
    let turn = outcome(
        vec![
            tool("old-denial", "failed"),
            tool("fallback", "completed"),
            tool("unrelated", "failed"),
        ],
        StopReason::EndTurn,
        "old-denial",
    );
    assert_eq!(turn["status"], "failed");
    assert_eq!(turn["error"]["code"], "acp_tool_failure");
}

#[test]
fn whitespace_and_replayed_tool_status_do_not_hide_a_silent_failure() {
    for recovery in [
        reply("  "),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"read", "status":"completed"}),
    ] {
        let turn = outcome(
            vec![
                tool("read", "completed"),
                tool("denied", "failed"),
                recovery,
            ],
            StopReason::EndTurn,
            "denied",
        );
        assert_eq!(turn["status"], "failed");
    }
}

#[test]
fn a_parallel_tool_started_before_the_denial_is_not_a_recovery() {
    let turn = outcome(
        vec![
            tool("parallel", "in_progress"),
            tool("denied", "failed"),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"parallel", "status":"completed"}),
        ],
        StopReason::EndTurn,
        "denied",
    );
    assert_eq!(turn["status"], "failed");
    assert_eq!(turn["error"]["code"], "acp_permission_denied");
}

#[test]
fn a_tool_started_after_the_denial_can_recover_when_it_finishes_later() {
    let turn = outcome(
        vec![
            tool("denied", "failed"),
            tool("fallback", "in_progress"),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"fallback", "status":"completed"}),
        ],
        StopReason::EndTurn,
        "denied",
    );
    assert_eq!(turn["status"], "completed");
}
