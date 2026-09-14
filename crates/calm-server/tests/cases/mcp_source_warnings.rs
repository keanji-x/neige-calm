//! #1669 §2.3 — receipt `warnings`: the `neige://source/` links in the
//! prose a write touched that this track cannot resolve, on each of the
//! five agent write doors (`calm.report.commit`, `calm.report.blocks.upsert`,
//! `calm.report.write_markdown`, `calm.report.write`, `calm.report.edit`),
//! malformed citations included. The write is never blocked.

#![cfg(unix)]

use calm_server::mcp_server::tools::source::TOOL_SOURCE_CAPTURE;
use calm_server::mcp_server::tools::track_report::{TOOL_REPORT_EDIT, TOOL_REPORT_WRITE};
use calm_server::mcp_server::tools::track_report_blocks::{
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_COMMIT, TOOL_REPORT_WRITE_MARKDOWN,
};
use serde_json::{Value, json};

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};

const TOOL_REPORT_READ: &str = "calm.report.read";

async fn doc_rev(boot: &Boot) -> u64 {
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), json!({}))
        .await
        .expect("read")["docRev"]
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
            "if_doc_rev": rev,
            "message": "cite",
            "ops": [{ "op": "upsert", "kind": "prose", "markdown": markdown }],
        }),
    )
    .await
    .expect("commit writes despite the dangling link");
    assert_eq!(receipt["docRev"], rev + 1);
    let blocks = receipt["blocks"].as_array().unwrap();
    let written = blocks.last().unwrap()["id"].as_str().unwrap();
    assert_eq!(
        receipt["warnings"],
        json!([warning(written, "neige://source/src_deadbeef")]),
        "{receipt}"
    );
}

#[tokio::test]
async fn blocks_upsert_reports_a_dangling_anchor() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let rev = doc_rev(&boot).await;
    let dangling =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q7"));
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": format!("# Cite\n\n[anchor]({dangling})\n"),
            "if_doc_rev": rev,
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
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({
            "id": id,
            "kind": "prose",
            "markdown": format!(
                "# Cite\n\n[anchor]({})\n",
                calm_types::report_source_links::format_source_destination(&source_id, Some("q1"))
            ),
            "if_rev": receipt["rev"],
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
    let rev = doc_rev(&boot).await;
    let resolved_source =
        calm_types::report_source_links::format_source_destination(&source_id, None);
    let resolved_anchor =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q1"));
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({
            "body": format!(
                "# One\n\n[a]({resolved_source})\n\n# Two\n\n[b]({resolved_anchor})\n"
            ),
            "if_doc_rev": rev,
        }),
    )
    .await
    .expect("write_markdown");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
    // The whole document is rescanned: a dangling link anywhere shows up,
    // attributed to its block.
    let rev = doc_rev(&boot).await;
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({
            "body": format!(
                "# One\n\n[a]({resolved_source})\n\n# Two\n\n[b](neige://source/src_00000000#q1)\n"
            ),
            "if_doc_rev": rev,
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
    let rev = doc_rev(&boot).await;
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": "# Cite\n\n[dead](neige://source/src_deadbeef)\n",
            "if_doc_rev": rev,
        }),
    )
    .await
    .expect("seed a dangling link");
    let rev = doc_rev(&boot).await;
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "if_doc_rev": rev, "message": "summary only", "summary": "new summary" }),
    )
    .await
    .expect("summary-only commit");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
}

/// Design §5 counterexample: `neige://source/src_dead` is not a valid id
/// and `#q0` is not a valid anchor; both must be warned about, not
/// silently dropped by the scanner.
#[tokio::test]
async fn malformed_ids_and_anchors_are_warned_about() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let rev = doc_rev(&boot).await;
    let bad_anchor =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q0"));
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "if_doc_rev": rev,
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

/// `calm.report.write` (whole body) and `calm.report.edit` (string
/// replace) are the two remaining planner doors into `commit_report_op`;
/// they carry the same `warnings` as the block tools.
#[tokio::test]
async fn report_write_and_edit_carry_warnings_too() {
    let boot = boot().await;
    let source_id = capture_manual(&boot).await;
    let resolved =
        calm_types::report_source_links::format_source_destination(&source_id, Some("q1"));
    let rev = doc_rev(&boot).await;
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": format!("# One\n\n[ok]({resolved}) and [dead](neige://source/src_deadbeef)\n"),
            "message": "write",
            "if_doc_rev": rev,
        }),
    )
    .await
    .expect("calm.report.write");
    let warnings = receipt["warnings"].as_array().expect("warnings on write");
    assert_eq!(warnings.len(), 1, "{receipt}");
    assert_eq!(warnings[0]["destination"], "neige://source/src_deadbeef");
    assert_eq!(warnings[0]["kind"], "unresolved_source_link");

    // Repair the dangling link in place: no warnings left.
    let rev = doc_rev(&boot).await;
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        planner_identity(&boot),
        json!({
            "old_string": "neige://source/src_deadbeef",
            "new_string": resolved,
            "message": "repair",
            "if_doc_rev": rev,
        }),
    )
    .await
    .expect("calm.report.edit");
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
    // …and slip a new dangling anchor in through the same door.
    let rev = doc_rev(&boot).await;
    let receipt = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        planner_identity(&boot),
        json!({
            "old_string": "[ok](",
            "new_string": "[gone](neige://source/src_00000000#q1) [ok](",
            "message": "cite",
            "if_doc_rev": rev,
        }),
    )
    .await
    .expect("calm.report.edit");
    let warnings = receipt["warnings"].as_array().expect("warnings on edit");
    assert_eq!(warnings.len(), 1, "{receipt}");
    assert_eq!(warnings[0]["destination"], "neige://source/src_00000000#q1");
}
