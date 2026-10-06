//! Receipt `warnings`: the `neige://source/` links in touched prose that this track cannot
//! resolve, on each of the agent write doors. The write is never blocked.

#![cfg(unix)]

use calm_server::mcp_server::tools::source::TOOL_SOURCE_CAPTURE;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_COMMIT;
use serde_json::{Value, json};

use crate::mcp_track_report::{
    Boot, boot, call_tool, planner_identity, read_then_write_markdown, upsert_block,
};

const TOOL_REPORT_READ: &str = "neige_report_read";

/// A full read: this session's anchor for the next write, and the docRev it saw.
async fn doc_rev(boot: &Boot) -> u64 {
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
        .await
        .expect("read")["doc_rev"]
        .as_u64()
        .expect("docRev")
}

/// A `manual` source with one anchor (`q1`), so a link can resolve.
async fn capture_manual(boot: &Boot) -> String {
    let receipt = call_tool(
        boot,
        TOOL_SOURCE_CAPTURE,
        planner_identity(boot),
        json!({
            "manual": { "text": "the quoted sentence and more" },
            "provenance": "manual",
            "title": "t",
            "quotes": ["quoted sentence"],
        }),
    )
    .await
    .expect("manual capture");
    assert_eq!(receipt["quotes"][0]["id"], "q1");
    receipt["source_id"].as_str().unwrap().to_string()
}

fn warning(block_id: &str, destination: &str) -> Value {
    json!({
        "kind": "unresolved_source_link",
        "block_id": block_id,
        "destination": destination,
    })
}

#[tokio::test]
async fn commit_reports_a_dangling_source_id_and_still_writes() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let rev = doc_rev(&boot).await;
    let markdown = format!(
        "# Sources\n\nsee [ok]({}) and [dead](neige://source/src_deadbeef) twice: \
         [dead2](neige://source/src_deadbeef)\n",
        calm_types::report_source_links::format_source_destination(&source_id, Some("q1"))
    );
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "message": "cite",
            "ops": [{ "op": "upsert", "kind": "prose", "markdown": markdown }],
        }),
    )
    .await
    .expect("commit writes despite the dangling link");
    assert_eq!(receipt["doc_rev"], rev + 1);
    let blocks = receipt["blocks"].as_array().unwrap();
    let written = blocks.last().unwrap()["id"].as_str().unwrap();
    assert_eq!(
        receipt["warnings"],
        json!([warning(written, "neige://source/src_deadbeef")]),
        "{receipt}"
    );
}

#[tokio::test]
async fn a_single_upsert_reports_a_dangling_anchor() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let dangling =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q7"));
    let receipt = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": format!("# Cite\n\n[anchor]({dangling})\n"),
        }),
    )
    .await
    .expect("upsert writes");
    let id = receipt["id"].as_str().unwrap();
    assert_eq!(
        receipt["warnings"],
        json!([warning(id, &dangling)]),
        "{receipt}"
    );
    // Replacing the block with a resolved link clears the warning; a
    // non-prose upsert carries an empty list.
    let receipt = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({
            "id": id,
            "kind": "prose",
            "markdown": format!(
                "# Cite\n\n[anchor]({})\n",
                calm_types::report_source_links::format_source_destination(&source_id, Some("q1"))
            ),
        }),
    )
    .await
    .expect("replace");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
}

#[tokio::test]
async fn write_markdown_scans_every_prose_block_and_resolved_links_warn_nothing() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let resolved_source =
        calm_types::report_source_links::format_source_destination(&source_id, None);
    let resolved_anchor =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q1"));
    let receipt = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": format!(
                "# One\n\n[a]({resolved_source})\n\n# Two\n\n[b]({resolved_anchor})\n"
            ),
        }),
    )
    .await
    .expect("write_markdown");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
    // The whole document is rescanned: a dangling link anywhere shows up,
    // attributed to its block.
    let receipt = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": format!(
                "# One\n\n[a]({resolved_source})\n\n# Two\n\n[b](neige://source/src_00000000#q1)\n"
            ),
        }),
    )
    .await
    .expect("write_markdown");
    let warnings = receipt["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{receipt}");
    assert_eq!(warnings[0]["destination"], "neige://source/src_00000000#q1");
    let read = call_tool(&boot, TOOL_REPORT_READ, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let two = read["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == warnings[0]["block_id"])
        .expect("the warned block exists");
    assert_eq!(two["kind"], "prose");
}

#[tokio::test]
async fn summary_only_commit_carries_no_warnings_even_with_dangling_links_in_place() {
    let boot = boot().await;
    upsert_block(
        &boot,
        planner_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": "# Cite\n\n[dead](neige://source/src_deadbeef)\n",
        }),
    )
    .await
    .expect("seed a dangling link");
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "summary only", "summary": "new summary" }),
    )
    .await
    .expect("summary-only commit");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
}

/// `neige://source/src_dead` is not a valid id and `#q0` is not a valid anchor; both must be
/// warned about, not silently dropped by the scanner.
#[tokio::test]
async fn malformed_ids_and_anchors_are_warned_about() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    doc_rev(&boot).await;
    let bad_anchor =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q0"));
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "message": "cite badly",
            "ops": [{
                "op": "upsert",
                "kind": "prose",
                "markdown": format!(
                    "# Sources\n\n[dead](neige://source/src_dead) [anchor]({bad_anchor})\n"
                ),
            }],
        }),
    )
    .await
    .expect("commit writes despite malformed links");
    let written = receipt["blocks"].as_array().unwrap().last().unwrap()["id"]
        .as_str()
        .unwrap();
    assert_eq!(
        receipt["warnings"],
        json!([
            warning(written, "neige://source/src_dead"),
            warning(written, &bad_anchor),
        ]),
        "{receipt}"
    );
}

/// A whole-document write that leaves a dangling link, then local commits that repair it and cite a
/// missing anchor: each receipt names exactly the links the write leaves unresolved.
#[tokio::test]
async fn warnings_follow_a_dangling_link_through_its_repair_and_a_new_citation() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let resolved =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q1"));
    let receipt = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": format!("# One\n\n[ok]({resolved}) and [dead](neige://source/src_deadbeef)\n"),
            "message": "write",
        }),
    )
    .await
    .expect("neige_report_write");
    let warnings = receipt["warnings"].as_array().expect("warnings on write");
    assert_eq!(warnings.len(), 1, "{receipt}");
    assert_eq!(warnings[0]["destination"], "neige://source/src_deadbeef");
    assert_eq!(warnings[0]["kind"], "unresolved_source_link");

    // Repair the dangling link in place: no warnings left.
    let receipt = commit_replacing_only_block(
        &boot,
        &format!("# One\n\n[ok]({resolved}) and [dead]({resolved})\n"),
        "repair",
    )
    .await;
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
    // …and slip a new dangling anchor in through the same door.
    let receipt = commit_replacing_only_block(
        &boot,
        &format!(
            "# One\n\n[gone](neige://source/src_00000000#q1) [ok]({resolved}) and [dead]({resolved})\n"
        ),
        "cite",
    )
    .await;
    let warnings = receipt["warnings"].as_array().expect("warnings on commit");
    assert_eq!(warnings.len(), 1, "{receipt}");
    assert_eq!(warnings[0]["destination"], "neige://source/src_00000000#q1");
}

/// Replace the report's only block through `neige_report_commit`; returns the receipt.
async fn commit_replacing_only_block(boot: &Boot, markdown: &str, message: &str) -> Value {
    let index = call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
        .await
        .expect("read");
    let blocks = index["blocks"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "{index}");
    call_tool(
        boot,
        TOOL_REPORT_COMMIT,
        planner_identity(boot),
        json!({
            "message": message,
            "ops": [{
                "op": "upsert", "id": blocks[0]["id"],
                "kind": "prose", "markdown": markdown
            }],
        }),
    )
    .await
    .expect("neige_report_commit")
}
