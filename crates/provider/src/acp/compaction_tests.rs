use super::translate::{TurnContext, TurnTranslator};
use serde_json::{Value, json};

fn translator() -> TurnTranslator {
    TurnTranslator::new(TurnContext {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        native_session_id: "session".into(),
    })
}
fn frame(update: Value) -> Value {
    json!({"sessionId":"session", "update":update})
}

#[test]
fn acp_compaction_rejects_malformed_updates() {
    for update in [
        json!({"sessionUpdate":"compaction_update","status":"completed"}),
        json!({"sessionUpdate":"compaction_update","compactionId":7,"status":"completed"}),
        json!({"sessionUpdate":"compaction_update","compactionId":"a","status":"unknown"}),
        json!({"sessionUpdate":"compaction_summary_chunk","content":{"type":"text","text":"orphan"}}),
    ] {
        assert!(
            translator().update(&frame(update.clone()), 0).is_err(),
            "{update}"
        );
    }
}

fn lifecycle(id: &str, status: &str) -> Value {
    json!({"sessionUpdate":"compaction_update","compactionId":id,"status":status})
}
fn chunk(id: &str, text: &str) -> Value {
    json!({"sessionUpdate":"compaction_summary_chunk","compactionId":id,"content":{"type":"text","text":text}})
}
#[test]
fn acp_compaction_interleaves_native_ids_and_replaces_final_summary() {
    use super::compaction::Status;
    let mut t = translator();
    for update in [
        lifecycle("manual", "in_progress"),
        lifecycle("auto", "in_progress"),
        chunk("manual", "one"),
        chunk("auto", "other"),
        chunk("manual", "two"),
    ] {
        assert!(t.update(&frame(update), 0).unwrap().is_empty());
    }
    assert_eq!(
        t.compactions()["manual"].summary,
        vec![
            json!({"type":"text","text":"one"}),
            json!({"type":"text","text":"two"})
        ]
    );
    assert_eq!(
        t.compactions()["auto"].summary,
        vec![json!({"type":"text","text":"other"})]
    );
    let mut done = lifecycle("manual", "completed");
    done["summary"] = json!([{"type":"text","text":"final"}]);
    t.update(&frame(done), 0).unwrap();
    assert_eq!(t.compactions()["manual"].status, Status::Completed);
    assert_eq!(
        t.compactions()["manual"].summary,
        vec![json!({"type":"text","text":"final"})]
    );
    assert_eq!(t.compactions()["auto"].status, Status::InProgress);
}
#[test]
fn acp_compaction_rejects_mismatched_and_settled_chunks_without_mutation() {
    let mut t = translator();
    t.update(&frame(lifecycle("a", "in_progress")), 0).unwrap();
    let before = t.compactions().clone();
    assert!(t.update(&frame(chunk("b", "misrouted")), 0).is_err());
    let mut wrong_session = frame(chunk("a", "wrong session"));
    wrong_session["sessionId"] = json!("other");
    assert!(t.update(&wrong_session, 0).is_err());
    assert_eq!(t.compactions(), &before);
    t.update(&frame(lifecycle("a", "completed")), 0).unwrap();
    assert!(t.update(&frame(chunk("a", "late")), 0).is_err());
}
#[test]
fn acp_compaction_replay_statuses_and_patch_semantics() {
    use super::compaction::Status;
    let mut t = translator();
    for (id, status, expected) in [
        ("running", "in_progress", Status::InProgress),
        ("replay", "completed", Status::Completed),
        ("cancel", "cancelled", Status::Cancelled),
        ("failure", "failed", Status::Failed),
    ] {
        t.update(&frame(lifecycle(id, status)), 0).unwrap();
        assert_eq!(t.compactions()[id].status, expected);
    }
    let mut failure = lifecycle("failure", "failed");
    failure["error"] = json!("native error");
    t.update(&frame(failure.clone()), 0).unwrap();
    t.update(&frame(lifecycle("failure", "failed")), 0).unwrap();
    assert_eq!(
        t.compactions()["failure"].error.as_deref(),
        Some("native error")
    );
    failure["error"] = Value::Null;
    t.update(&frame(failure), 0).unwrap();
    assert_eq!(t.compactions()["failure"].error, None);
    t.update(&frame(chunk("running", "partial")), 0).unwrap();
    t.update(&frame(lifecycle("running", "completed")), 0)
        .unwrap();
    assert_eq!(t.compactions()["running"].summary.len(), 1);
    for summary in [Value::Null, json!([])] {
        let mut patch = lifecycle("running", "completed");
        patch["summary"] = summary;
        t.update(&frame(patch), 0).unwrap();
        assert!(t.compactions()["running"].summary.is_empty());
    }
}
#[test]
fn acp_compaction_typed_decode_and_invalid_payloads() {
    use super::compaction::{CompactionUpdate, Patch, Status};
    assert_eq!(
        CompactionUpdate::decode(&lifecycle("opaque/id", "completed")).unwrap(),
        Some(CompactionUpdate::Lifecycle {
            compaction_id: "opaque/id".into(),
            status: Status::Completed,
            summary: Patch::Unchanged,
            error: Patch::Unchanged
        })
    );
    assert_eq!(
        CompactionUpdate::decode(&chunk("a", "text")).unwrap(),
        Some(CompactionUpdate::SummaryChunk {
            compaction_id: "a".into(),
            content: json!({"type":"text","text":"text"})
        })
    );
    for update in [
        json!({"sessionUpdate":"compaction_update","compactionId":"a","status":"completed","summary":"wrong"}),
        json!({"sessionUpdate":"compaction_update","compactionId":"a","status":"completed","error":"wrong"}),
        json!({"sessionUpdate":"compaction_update","compactionId":"a","status":"failed","error":{}}),
        json!({"sessionUpdate":"compaction_update","compactionId":"a","status":"in_progress","summary":[{"type":"text","text":"early"}]}),
        json!({"sessionUpdate":"compaction_summary_chunk","compactionId":"a","content":{"type":"text"}}),
    ] {
        assert!(CompactionUpdate::decode(&update).is_err(), "{update}");
    }
}
#[test]
fn acp_compaction_preserves_translation_and_ignores_legacy_markers() {
    use crate::events::PlannerEventKind;
    let mut t = translator();
    let mut baseline = translator();
    let text = frame(
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"reply"}}),
    );
    assert_eq!(
        format!("{:?}", t.update(&text, 0).unwrap()),
        format!("{:?}", baseline.update(&text, 0).unwrap())
    );
    for update in [
        lifecycle("auto", "in_progress"),
        lifecycle("auto", "failed"),
        json!({"sessionUpdate":"session_info_update","_meta":{"opencode/compaction":{"status":"started"}}}),
        json!({"sessionUpdate":"future_kind"}),
    ] {
        assert!(t.update(&frame(update), 0).unwrap().is_empty());
    }
    assert_eq!(t.compactions().len(), 1);
    let finished = t.finish(super::protocol::StopReason::EndTurn, 1);
    assert!(
        matches!(&finished.last().unwrap().kind, PlannerEventKind::TurnCompleted {turn} if turn["status"]=="completed")
    );
    assert_eq!(
        format!("{finished:?}"),
        format!(
            "{:?}",
            baseline.finish(super::protocol::StopReason::EndTurn, 1)
        )
    );
}
