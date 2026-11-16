//! Production-boundary contracts for ordered output, retained budgets and observation-only seals.
use super::ordered_output_tests::{chunk, completed, translator};
use super::{
    content::{self, ContentContext},
    protocol::StopReason,
};
use crate::events::{ItemPhase, PlannerEvent, PlannerEventKind};
use calm_types::output_content::OutputContentV1;
use serde_json::{Value, json};

fn update(
    t: &mut super::translate::TurnTranslator,
    update: Value,
) -> Result<Vec<PlannerEvent>, super::Error> {
    t.update(&json!({"sessionId":"session","update":update}), 1)
}
fn tool(t: &mut super::translate::TurnTranslator, value: Value) -> Vec<PlannerEvent> {
    update(t, value).unwrap()
}
fn decode(v: &Value) -> Result<calm_types::output_content::OutputBlock, super::Error> {
    content::decode(v, ContentContext::OutputRetained)
}
fn image() -> Value {
    json!({"type":"image","mimeType":"image/png","data":"AAAA"})
}

#[test]
fn five_wire_blocks_preserve_optional_presence_and_nested_metadata() {
    for value in [
        json!({"type":"text","text":"hello","annotations":{"audience":["assistant","user"],"priority":0.5,"lastModified":"date","_meta":null},"_meta":{"k":1},"chunkMeta":"wire extension"}),
        json!({"type":"image","mimeType":"image/png","data":"AA==","uri":null,"_meta":null}),
        json!({"type":"audio","mimeType":"audio/wav","data":"AAA=","annotations":null}),
        json!({"type":"resource_link","name":"r","uri":"file:///inert","size":42,"title":null,"description":"d","mimeType":null}),
        json!({"type":"resource","annotations":null,"resource":{"uri":"urn:r","text":"body","mimeType":"text/html","_meta":{"x":true}}}),
        json!({"type":"resource","_meta":{},"resource":{"uri":"urn:r","blob":"AA==","mimeType":null,"_meta":null}}),
    ] {
        let block = decode(&value).unwrap();
        assert_eq!(content::wire_block(&block), value);
        let typed = OutputContentV1::new(vec![block]);
        let restored: OutputContentV1 =
            serde_json::from_value(serde_json::to_value(&typed).unwrap()).unwrap();
        assert_eq!(restored, typed);
    }
}
#[test]
fn known_bad_required_optional_and_base64_are_protocol_errors() {
    for value in [
        json!({"type":"text"}),
        json!({"type":"text","text":null}),
        json!({"type":"image","mimeType":"image/png"}),
        json!({"type":"image","data":"AA=="}),
        json!({"type":"audio","data":"AA==","mimeType":3}),
        json!({"type":"resource_link","uri":"urn:r"}),
        json!({"type":"resource_link","name":"n"}),
        json!({"type":"resource","resource":{"uri":"urn:r"}}),
        json!({"type":"resource","resource":{"uri":"urn:r","text":"","blob":""}}),
        json!({"type":"resource","resource":{"uri":null,"text":""}}),
        json!({"type":"text","text":"","annotations":{"audience":["system"]}}),
        json!({"type":"text","text":"","annotations":{"priority":"high"}}),
        json!({"type":"text","text":"","_meta":[]}),
        json!({"type":"resource_link","name":"n","uri":"u","size":-1}),
    ] {
        assert!(
            matches!(decode(&value), Err(super::Error::Protocol(_))),
            "{value}"
        );
    }
    for data in ["A", "A===", "AB==", "AAB=", "AA==AAAA", "AA-_", "AA\n="] {
        assert!(matches!(
            decode(&json!({"type":"audio","data":data,"mimeType":"audio/x"})),
            Err(super::Error::Protocol(_))
        ));
    }
}
#[test]
fn unknown_extensions_are_inert_bounded_and_not_known_fallbacks() {
    let raw = json!({"type":"future","data":"invalid base64","uri":"file:///do-not-read","_meta":{"html":"<script>"}});
    let mut t = translator();
    chunk(&mut t, raw.clone());
    let items = completed(&t.finish(StopReason::EndTurn, 2));
    assert_eq!(items[0]["outputContent"]["blocks"][0]["raw"], raw);
    assert_eq!(items[0]["text"], "");
    assert!(decode(&json!({"type":"future","data":"x".repeat(65536)})).is_err());
    let mut deep = json!(null);
    for _ in 0..34 {
        deep = json!({"x":deep});
    }
    assert!(decode(&json!({"type":"text","text":"","_meta":deep})).is_err());
}
#[test]
fn media_only_repeats_and_text_order_survive_final_and_partial() {
    for partial in [false, true] {
        let mut t = translator();
        for block in [image(), image(), json!({"type":"text","text":"tail"})] {
            chunk(&mut t, block);
        }
        let events = if partial {
            t.materialize_partial(3)
        } else {
            t.finish(StopReason::EndTurn, 3)
        };
        let items = completed(&events);
        assert_eq!(
            items[0]["outputContent"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(items[0]["text"], "tail");
        assert_eq!(events.iter().any(|e|matches!(&e.kind,PlannerEventKind::Item{params,..} if params["_partial"] == true)),partial);
        if partial {
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e.kind, PlannerEventKind::TurnCompleted { .. }))
            );
        }
        assert!(t.materialize_partial(4).is_empty());
        assert!(t.finish(StopReason::EndTurn, 4).is_empty());
        assert!(
            update(
                &mut t,
                json!({"sessionUpdate":"agent_message_chunk","content":image()})
            )
            .is_err()
        );
    }
    let mut t = translator();
    chunk(&mut t, image());
    assert_eq!(
        completed(&t.finish(StopReason::EndTurn, 3))[0]["outputContent"]["blocks"][0]["type"],
        "image"
    );
}
#[test]
fn affiliation_channel_and_empty_chunks_create_fresh_continuous_segments() {
    let mut t = translator();
    let mut events = Vec::new();
    for (kind, id) in [
        ("agent_message_chunk", Some("M")),
        ("agent_thought_chunk", Some("M")),
        ("agent_message_chunk", Some("M")),
        ("agent_message_chunk", None),
        ("agent_message_chunk", Some("M")),
    ] {
        let mut frame = json!({"sessionUpdate":kind,"content":{"type":"text","text":""}});
        if let Some(id) = id {
            frame["messageId"] = json!(id);
        }
        events.extend(update(&mut t, frame).unwrap());
    }
    events.extend(t.finish(StopReason::EndTurn, 2));
    let items = completed(&events);
    assert_eq!(items.len(), 5);
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item["outputSegment"]["ordinal"], (i + 1) as u64);
    }
    assert_eq!(items[1]["type"], "reasoning");
    let ids: std::collections::BTreeSet<_> =
        items.iter().map(|v| v["id"].clone().to_string()).collect();
    assert_eq!(ids.len(), 5);
}
#[test]
fn metadata_changes_prevent_merge_without_sealing_and_preserve_null() {
    let mut t = translator();
    let mut events = Vec::new();
    for meta in [
        None,
        Some(Value::Null),
        Some(json!({"x":1})),
        Some(json!({"x":1})),
    ] {
        let mut frame = json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x","_meta":null,"chunkMeta":"wire"}});
        if let Some(meta) = meta {
            frame["_meta"] = meta;
        }
        events.extend(update(&mut t, frame).unwrap());
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.kind,
                PlannerEventKind::Item {
                    phase: ItemPhase::Started,
                    ..
                }
            ))
            .count(),
        1
    );
    let items = completed(&t.finish(StopReason::EndTurn, 2));
    let blocks = items[0]["outputContent"]["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[2]["text"], "xx");
    assert_eq!(blocks[1]["chunk"]["meta"], Value::Null);
    assert_eq!(blocks[1]["fields"]["chunkMeta"], "wire");
}
#[test]
fn chunking_does_not_change_retained_content_or_persist_each_snapshot() {
    for (count, size) in [(400, 1024), (1, 409600), (4097, 1)] {
        let mut t = translator();
        let mut starts = 0;
        let mut delta = String::new();
        for _ in 0..count {
            for e in chunk(&mut t, json!({"type":"text","text":"x".repeat(size)})) {
                match e.kind {
                    PlannerEventKind::Item {
                        phase: ItemPhase::Started,
                        params,
                        ..
                    } => {
                        starts += 1;
                        assert_eq!(params["item"]["outputContent"]["blocks"], json!([]));
                    }
                    PlannerEventKind::ReplyDelta { delta: d, .. } => delta.push_str(&d),
                    _ => panic!("unexpected observation"),
                }
            }
            assert!(t.flush().is_empty());
        }
        assert_eq!(starts, 1);
        let events = t.finish(StopReason::EndTurn, 3);
        let items = completed(&events);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], delta);
        assert_eq!(delta.len(), count * size);
        assert_eq!(
            items[0]["outputContent"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(t.finish(StopReason::EndTurn, 4).is_empty());
    }
}
#[test]
fn rejected_content_or_tool_candidate_does_not_seal_or_advance_identity() {
    let mut t = translator();
    chunk(&mut t, json!({"type":"text","text":"before"}));
    for bad in [
        json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"image","data":"bad","mimeType":"x"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"bad","title":"Read","content":[{"type":"content","content":{"type":"text"}}]}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"missing"}),
    ] {
        assert!(update(&mut t, bad).is_err());
    }
    chunk(&mut t, json!({"type":"text","text":"after"}));
    let items = completed(&t.finish(StopReason::EndTurn, 2));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "beforeafter");
    assert_eq!(items[0]["outputSegment"]["ordinal"], 1);
}
#[test]
fn retained_text_budget_counts_body_once_across_seals_and_rejections() {
    let mut t = translator();
    chunk(
        &mut t,
        json!({"type":"text","text":"x".repeat(5*1024*1024)}),
    );
    tool(
        &mut t,
        json!({"sessionUpdate":"tool_call","toolCallId":"t","title":"Read"}),
    );
    for _ in 0..11 {
        chunk(&mut t, json!({"type":"text","text":"\"".repeat(1024*1024)}));
    }
    assert!(
        update(
            &mut t,
            json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"x"}})
        )
        .is_err()
    );
    let items = completed(&t.finish(StopReason::EndTurn, 2));
    assert_eq!(items[0]["text"].as_str().unwrap().len(), 11 * 1024 * 1024);
}
#[test]
fn tool_replacement_releases_retained_media_and_text_without_charging_copies() {
    let mut t = translator();
    let media = json!({"type":"image","mimeType":"image/png","data":"AAAA".repeat(1024*1024)});
    let patch = |content: Value| json!({"sessionUpdate":"tool_call_update","toolCallId":"t","content":content});
    tool(
        &mut t,
        json!({"sessionUpdate":"tool_call","toolCallId":"t","title":"Read","name":"native_name","content":[{"type":"content","content":media}]}),
    );
    for _ in 0..3 {
        tool(&mut t, patch(json!([{"type":"content","content":media}])));
    }
    chunk(&mut t, json!({"type":"text","text":"kept"}));
    assert!(
        update(
            &mut t,
            json!({"sessionUpdate":"agent_thought_chunk","content":media})
        )
        .is_err()
    );
    // Null preserves the current value and seals only after acceptance.
    let preserved = completed(&tool(&mut t, patch(Value::Null)));
    assert_eq!(preserved[0]["text"], "kept");
    let cleared = tool(&mut t, patch(json!([])));
    let PlannerEventKind::Item { params, .. } = &cleared[0].kind else {
        panic!()
    };
    assert_eq!(params["item"]["outputContent"]["blocks"], json!([]));
    assert_eq!(params["item"]["native"]["name"], "native_name");
    chunk(&mut t, media);
    assert_eq!(
        completed(&t.finish(StopReason::EndTurn, 2))[0]["outputContent"]["blocks"][0]["type"],
        "image"
    );
}
#[test]
fn tool_diff_presence_and_content_codec_survive_projection() {
    for old in [None, Some(Value::Null), Some(json!(""))] {
        let mut t = translator();
        let mut diff = json!({"type":"diff","path":"p","newText":"new"});
        if let Some(old) = old.clone() {
            diff["oldText"] = old;
        }
        let events = tool(
            &mut t,
            json!({"sessionUpdate":"tool_call","toolCallId":"t","title":"Edit","content":[diff,{"type":"terminal","terminalId":"term"},{"type":"content","content":image()}]}),
        );
        let PlannerEventKind::Item { params, .. } = &events[0].kind else {
            panic!()
        };
        let blocks = &params["item"]["outputContent"]["blocks"];
        assert_eq!(blocks[0]["fields"].get("oldText"), old.as_ref());
        assert_eq!(blocks[1]["type"], "terminal_reference");
        assert_eq!(blocks[2]["type"], "image");
    }
}
#[test]
fn compaction_shares_structural_codec_but_not_output_budgets() {
    let mut t = translator();
    let summary = json!({"type":"text","text":"summary","_meta":{"large":"x".repeat(70000)}});
    update(&mut t,json!({"sessionUpdate":"compaction_update","compactionId":"c","status":"completed","summary":[summary.clone()]})).unwrap();
    assert_eq!(t.compactions()["c"].summary[0], summary);
    assert!(decode(&summary).is_err());
    assert!(update(&mut t,json!({"sessionUpdate":"compaction_update","compactionId":"c","status":"completed","summary":[{"type":"image","data":"bad","mimeType":"image/png"}]})).is_err());
    assert_eq!(t.compactions()["c"].summary[0], summary);
}
#[test]
fn prompt_keeps_existing_image_capability_and_frame_contract() {
    let caps = super::protocol::AgentCapabilities {
        prompt_capabilities: super::protocol::PromptCapabilities {
            image: true,
            embedded_context: false,
        },
        ..Default::default()
    };
    let blocks = [super::protocol::ContentBlock::Image {
        data: "AAAA".repeat(1024 * 1024),
        mime_type: "image/png".into(),
    }];
    assert!(super::protocol::validate_prompt(&caps, "session", &blocks).is_ok());
}

#[test]
fn abnormal_terminal_uses_the_same_complete_content_materializer() {
    let mut results = Vec::new();
    for mode in 0..3 {
        let mut t = translator();
        for block in [
            json!({"type":"text","text":"before"}),
            image(),
            json!({"type":"text","text":"after"}),
        ] {
            chunk(&mut t, block);
        }
        let events = match mode {
            0 => t.finish(StopReason::EndTurn, 3),
            1 => t.fail("remote failure", 3),
            _ => t.finish(StopReason::Cancelled, 3),
        };
        let item = completed(&events).remove(0);
        results.push(item);
        assert!(
            matches!(&events.last().unwrap().kind,PlannerEventKind::TurnCompleted{turn} if turn["status"]==["completed","failed","interrupted"][mode])
        );
        assert!(t.fail("again", 4).is_empty());
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0], results[2]);
}
#[test]
fn sealed_media_stays_charged_and_rejection_preserves_the_open_segment() {
    let mut t = translator();
    let media = json!({"type":"audio","mimeType":"audio/x","data":"AAAA".repeat(768*1024)});
    chunk(&mut t, media.clone());
    let boundary = update(
        &mut t,
        json!({"sessionUpdate":"agent_thought_chunk","content":media.clone()}),
    )
    .unwrap();
    assert_eq!(
        completed(&boundary)[0]["outputContent"]["blocks"][0]["type"],
        "audio"
    );
    assert!(
        update(
            &mut t,
            json!({"sessionUpdate":"agent_message_chunk","content":media})
        )
        .is_err()
    );
    let items = completed(&t.finish(StopReason::EndTurn, 3));
    assert_eq!(items[0]["outputSegment"]["ordinal"], 2);
    assert_eq!(items[0]["type"], "reasoning");
    assert_eq!(
        items[0]["outputContent"]["blocks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
