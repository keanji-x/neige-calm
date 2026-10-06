//! `neige_report_read` selects with the §4 vocabulary (#2087 B2): top-level `blocks` / `sections`
//! (the names `neige_track_cat` uses) and `detail: full | index`. Each retired `select` shape maps
//! to one of them with the same text and the same write anchors:
//!
//! | retired `select`             | now                    |
//! |------------------------------|------------------------|
//! | absent, `null`, `"full"`     | absent, `detail: full` |
//! | `"index"`                    | `detail: index`        |
//! | `{ blocks: [ids] }`          | `blocks: [ids]`        |
//! | `{ sections: [H1 texts] }`   | `sections: [H1 texts]` |

use crate::mcp_track_report::{
    Boot, boot, call_tool, call_tool_raw, current_doc_rev, planner_identity, seed_large_body,
};
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::plugin_host::mcp::RpcError;
use calm_server::report_read_ledger::LastRead;
use serde_json::{Value, json};

/// A planner session that has read nothing yet, so the ledger shows exactly this one read.
fn fresh_reader(boot: &Boot, name: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        session_id: format!("b2-reader-{name}"),
        ..planner_identity(boot)
    }
}

fn anchors(boot: &Boot, reader: &ToolCallIdentity) -> Option<LastRead> {
    boot.ctx
        .read_ledger
        .last_read(&reader.session_id, boot.report_card_id.as_str())
}

async fn block_ids(boot: &Boot) -> Vec<String> {
    let index = call_tool(
        boot,
        TOOL_REPORT_READ,
        fresh_reader(boot, "ids"),
        json!({"detail": "index"}),
    )
    .await
    .unwrap();
    index["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn detail_index_returns_anchors_without_text_and_records_no_read() {
    let boot = boot().await;
    seed_large_body(&boot).await;
    let reader = fresh_reader(&boot, "index");
    let wire = call_tool_raw(
        &boot,
        TOOL_REPORT_READ,
        reader.clone(),
        json!({"detail": "index"}),
    )
    .await
    .expect("planner reads the index");
    let out = &wire["structuredContent"];
    assert!(
        out.get("text").is_none(),
        "index must not carry text: {out}"
    );
    assert!(out.get("body").is_none(), "{out}");
    assert_eq!(out["doc_rev"].as_u64(), Some(current_doc_rev(&boot).await));
    let blocks = out["blocks"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 3);
    for block in blocks {
        assert!(block["id"].is_string() && block["kind"].is_string() && block["rev"].is_u64());
    }
    assert_eq!(out["summary"], "a large report");
    assert!(out.get("task_diagnostics").is_some(), "{out}");
    let line = wire["content"][0]["text"].as_str().unwrap();
    assert!(line.contains(" · index only · "), "{line}");
    assert!(line.len() < 300, "{line}");
    assert_eq!(
        anchors(&boot, &reader),
        None,
        "an index read anchors nothing"
    );
}

/// The default, `detail: "full"` and `detail: null` are one read: the whole body, every block
/// anchored and the read whole.
#[tokio::test]
async fn full_detail_is_the_default_read_and_anchors_every_block() {
    let boot = boot().await;
    let body = seed_large_body(&boot).await;
    let ids = block_ids(&boot).await;
    for (name, args) in [
        ("default", json!({})),
        ("full", json!({"detail": "full"})),
        ("null", json!({"detail": null})),
    ] {
        let reader = fresh_reader(&boot, name);
        let out = call_tool(&boot, TOOL_REPORT_READ, reader.clone(), args)
            .await
            .unwrap();
        assert_eq!(out["text"].as_str(), Some(body.as_str()), "{name}");
        let read = anchors(&boot, &reader).expect("a text read anchors");
        assert!(read.whole, "{name}");
        let mut seen: Vec<String> = read.blocks.keys().cloned().collect();
        seen.sort();
        let mut want = ids.clone();
        want.sort();
        assert_eq!(seen, want, "{name}");
    }
}

#[tokio::test]
async fn blocks_return_only_those_blocks_in_document_order_and_anchor_only_them() {
    let boot = boot().await;
    seed_large_body(&boot).await;
    let ids = block_ids(&boot).await;
    let (b1, b2, b3) = (&ids[0], &ids[1], &ids[2]);
    let reader = fresh_reader(&boot, "blocks");
    let out = call_tool(
        &boot,
        TOOL_REPORT_READ,
        reader.clone(),
        json!({"blocks": [b2, b1]}),
    )
    .await
    .expect("planner reads two blocks");
    let text = out["text"].as_str().expect("text");
    let m1 = calm_types::report_blocks::marker_line(b1);
    let m2 = calm_types::report_blocks::marker_line(b2);
    assert!(text.starts_with(&m1), "{text:.200}");
    let second = text.find(&m2).expect("b2 marker present");
    assert!(text[..second].contains("# One"), "{text:.200}");
    assert!(text[second..].contains("# Two"));
    assert!(!text.contains("# Three"), "b3 must not be delivered");
    assert!(!text.contains(&calm_types::report_blocks::marker_line(b3)));
    assert_eq!(text.matches("<!-- neige:b_").count(), 2, "{text:.300}");
    assert_eq!(
        out["blocks"].as_array().map(Vec::len),
        Some(3),
        "the index stays whole"
    );
    assert_eq!(out["doc_rev"].as_u64(), Some(current_doc_rev(&boot).await));
    let read = anchors(&boot, &reader).expect("a blocks read anchors");
    assert!(!read.whole);
    let mut seen: Vec<&String> = read.blocks.keys().collect();
    seen.sort();
    assert_eq!(seen, vec![b1, b2]);

    // `with_markers` is a no-op on this form (markers are already on); `detail: full` is the default.
    let again = call_tool(
        &boot,
        TOOL_REPORT_READ,
        fresh_reader(&boot, "blocks-again"),
        json!({"blocks": [b1, b2], "with_markers": true, "detail": "full"}),
    )
    .await
    .unwrap();
    assert_eq!(again["text"], out["text"]);

    let err = call_tool(
        &boot,
        TOOL_REPORT_READ,
        planner_identity(&boot),
        json!({"blocks": [b1, "b_doesnotexist"]}),
    )
    .await
    .expect_err("unknown block id must be refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS, "{err:?}");
    assert!(err.message.contains("b_doesnotexist"), "{err:?}");
    assert!(
        err.message.contains(&format!("blocks are:\n  {b1}  One")),
        "the refusal lists the report's `<id>  <heading>` blocks: {err:?}"
    );
}

#[tokio::test]
async fn sections_return_and_anchor_whole_h1_sections() {
    let boot = boot().await;
    seed_large_body(&boot).await;
    let ids = block_ids(&boot).await;
    let reader = fresh_reader(&boot, "sections");
    let out = call_tool(
        &boot,
        TOOL_REPORT_READ,
        reader.clone(),
        json!({"sections": ["Two"]}),
    )
    .await
    .unwrap();
    // Checked first: another session of the same card replaces this one in the ledger.
    let read = anchors(&boot, &reader).expect("a sections read anchors");
    assert!(!read.whole);
    assert_eq!(read.blocks.keys().collect::<Vec<_>>(), vec![&ids[1]]);
    assert_eq!(read.sections.keys().collect::<Vec<_>>(), vec!["Two"]);
    let blocks_text = call_tool(
        &boot,
        TOOL_REPORT_READ,
        fresh_reader(&boot, "same-as-blocks"),
        json!({"blocks": [&ids[1]]}),
    )
    .await
    .unwrap();
    assert_eq!(
        out["text"], blocks_text["text"],
        "a section reads as its blocks"
    );
}

/// The retired `select` is refused with the valid keys, never ignored; a malformed selection or
/// detail names the key it is about.
#[tokio::test]
async fn retired_select_and_malformed_selections_are_refused() {
    let boot = boot().await;
    seed_large_body(&boot).await;
    let b1 = block_ids(&boot).await.remove(0);
    for old in [
        json!({"select": "index"}),
        json!({"select": "full"}),
        json!({"select": {"blocks": [&b1]}}),
        json!({"select": {"sections": ["One"]}}),
    ] {
        let err = call_tool(
            &boot,
            TOOL_REPORT_READ,
            planner_identity(&boot),
            old.clone(),
        )
        .await
        .expect_err("the retired select must be refused");
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "{old}");
        assert_eq!(
            err.message,
            "neige_report_read: unknown argument `select`; valid: blocks, detail, resolve, \
             sections, with_markers",
            "{old}"
        );
    }
    for (bad, key) in [
        (json!({"detail": "everything"}), "detail"),
        (json!({"detail": 7}), "detail"),
        (json!({"blocks": []}), "blocks"),
        (json!({"blocks": [1]}), "blocks"),
        (json!({"blocks": [&b1], "extra": true}), "extra"),
        (json!({"blocks": [&b1], "sections": ["One"]}), "not both"),
        (json!({"blocks": [&b1], "detail": "index"}), "detail"),
    ] {
        let err = call_tool(
            &boot,
            TOOL_REPORT_READ,
            planner_identity(&boot),
            bad.clone(),
        )
        .await
        .expect_err("a malformed read must be refused");
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "{bad} → {err:?}");
        assert!(err.message.contains(key), "{bad} → {err:?}");
    }
    let _: Value = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .expect("the default read still works");
}
