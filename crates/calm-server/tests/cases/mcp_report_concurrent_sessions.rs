//! Two real assistant conversations interleaving on one track's report: both read the same
//! revision from their own `calm.report.read`, A writes, B's write is refused with `-32001` and
//! must have written nothing. Only the CRDT bytes, the read projection and the event-log length
//! are compared; that is sound only while every projection write shares the report-write transaction.

#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, assistant_b_identity, assistant_identity, boot, call_tool, planner_identity,
};
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::track_report_blocks::{
    RPC_REV_CONFLICT, TOOL_REPORT_BLOCKS_MOVE, TOOL_REPORT_BLOCKS_UPSERT,
    TOOL_REPORT_WRITE_MARKDOWN,
};
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "calm.report.read";

/// What one session sees when it reads the report for itself, with block markers in the text.
async fn read_as(boot: &Boot, identity: ToolCallIdentity) -> Value {
    call_tool(
        boot,
        TOOL_REPORT_READ,
        identity,
        json!({"with_markers": true}),
    )
    .await
    .expect("assistant may read the report (§3.7 / G-B3)")
}

/// Everything a write could have disturbed. Comparing the raw `body_crdt` blob is the
/// load-bearing part: the read projection could round-trip a change away, the bytes cannot.
struct Persisted {
    crdt: Option<Vec<u8>>,
    read: Value,
    events: usize,
}

async fn persisted(boot: &Boot) -> Persisted {
    let (_, crdt) = boot
        .repo
        .card_get_with_body_crdt(boot.report_card_id.as_str())
        .await
        .expect("read report card")
        .expect("report card exists");
    Persisted {
        crdt,
        read: read_as(boot, assistant_identity(boot)).await,
        events: boot
            .repo
            .events_since(0, i64::MAX)
            .await
            .expect("read event log")
            .len(),
    }
}

/// After the refused write, the document is what A left behind, byte for byte, and nothing was logged.
fn assert_untouched(after_a: &Persisted, after_b: &Persisted, mouth: &str) {
    assert_eq!(
        after_a.crdt, after_b.crdt,
        "{mouth}: B's rejected write must leave the stored CRDT document \
         byte-identical to what A committed — a rev conflict that still \
         writes is exactly the silent overwrite §7 forbids"
    );
    assert_eq!(
        after_a.read, after_b.read,
        "{mouth}: the read projection (text, docRev, block revs, summary, \
         updated_at) must be unchanged by B's rejected write"
    );
    assert_eq!(
        after_a.events, after_b.events,
        "{mouth}: a rejected write must emit no events — the transaction \
         aborts before the sink commits"
    );
}

/// Two H1 sections of plain prose (no task fences, so only a conflict is under test).
async fn seed(boot: &Boot) {
    call_tool(
        boot,
        TOOL_REPORT_WRITE,
        planner_identity(boot),
        json!({
            "body": "# A\n\nalpha\n\n# B\n\nbeta\n",
            "summary": "seeded",
            "message": "seed",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("planner seeds the report");
}

fn blocks(read: &Value) -> &Vec<Value> {
    read["blocks"].as_array().expect("read returns block index")
}

fn doc_rev(read: &Value) -> u64 {
    read["docRev"].as_u64().expect("docRev is numeric")
}

fn assert_same_starting_point(a: &Value, b: &Value) {
    assert_eq!(
        doc_rev(a),
        doc_rev(b),
        "the two sessions must be racing from the same docRev; if they are \
         not, the later write is merely late, not interleaved"
    );
    assert_eq!(
        blocks(a),
        blocks(b),
        "the two sessions must see the same block index + revs"
    );
}

/// The two identities must be distinct on both axes the recorder gate resolves (card → role
/// and track; session → card): one session re-using an invalidated rev conflicts identically,
/// so nothing else in this file would notice if the two writers collapsed into one.
fn assert_two_distinct_conversations(a: &ToolCallIdentity, b: &ToolCallIdentity) {
    assert_ne!(
        a.card_id, b.card_id,
        "A and B must be two different assistant cards — one card writing \
         twice is the single-session case the existing conflict tests \
         already cover, and this file would then prove nothing new"
    );
    assert_ne!(
        a.session_id, b.session_id,
        "A and B must be two different sessions — §3.3's claim is about two \
         conversations interleaving, and one session cannot express it"
    );
}

/// Both sessions read for themselves: two conversations, same starting revision.
async fn read_both(boot: &Boot) -> (Value, Value) {
    let a = assistant_identity(boot);
    let b = assistant_b_identity(boot);
    assert_two_distinct_conversations(&a, &b);
    let a_read = read_as(boot, a).await;
    let b_read = read_as(boot, b).await;
    assert_same_starting_point(&a_read, &b_read);
    (a_read, b_read)
}

/// -32001 is shared by the `if_rev` and `if_doc_rev` comparators; `detail` is the fragment only
/// one of them can produce, including the exact stale rev B was holding.
fn assert_rev_conflict(err: calm_server::plugin_host::mcp::RpcError, mouth: &str, detail: &str) {
    assert_eq!(
        err.code, RPC_REV_CONFLICT,
        "{mouth}: the second writer must get -32001 (rev conflict), got: {err:?}"
    );
    assert!(
        err.message.contains(detail),
        "{mouth}: the refusal must be the one this mouth's CAS produces, \
         naming the stale rev B held — expected to find {detail:?} in: {}",
        err.message
    );
}

#[tokio::test]
async fn two_assistant_sessions_replacing_one_block_second_writer_gets_rev_conflict() {
    let boot = boot().await;
    seed(&boot).await;

    // 1. Both sessions read for themselves; neither rev below is typed by this test.
    let (a_read, b_read) = read_both(&boot).await;

    let a_target = blocks(&a_read)[0].clone();
    let b_target = blocks(&b_read)[0].clone();
    assert_eq!(
        a_target["id"], b_target["id"],
        "same block, by construction"
    );

    // 2. A writes with the rev A read.
    let a_out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "id": a_target["id"],
            "kind": "prose",
            "payload": {"markdown": "# A\n\nalpha, as A revised it\n"},
            "if_rev": a_target["rev"],
        }),
    )
    .await
    .expect("A's write lands: it is holding the current rev");
    assert_ne!(
        a_out["rev"], a_target["rev"],
        "a successful replace advances the block rev — otherwise B's rev \
         would still be current and this test would prove nothing"
    );
    let after_a = persisted(&boot).await;

    // 3. B writes with the rev B read, which A has just invalidated.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_b_identity(&boot),
        json!({
            "id": b_target["id"],
            "kind": "prose",
            "payload": {"markdown": "# A\n\nalpha, as B would have it\n"},
            "if_rev": b_target["rev"],
        }),
    )
    .await
    .expect_err("B is writing over A's change with a stale block rev");
    // The block-level comparator, not the document-level one that shares the code.
    assert_rev_conflict(
        err,
        "blocks.upsert",
        &format!(
            "rev conflict on block {}: current rev is {}, expected if_rev {}",
            b_target["id"].as_str().expect("block id is a string"),
            a_out["rev"],
            b_target["rev"],
        ),
    );

    // 4. And B's bytes are nowhere.
    let after_b = persisted(&boot).await;
    assert_untouched(&after_a, &after_b, "blocks.upsert");
    assert!(
        after_b.read["text"]
            .as_str()
            .expect("read returns text")
            .contains("as A revised it"),
        "A's content, not B's, is what the document holds"
    );
    assert!(
        !after_b.read["text"]
            .as_str()
            .expect("read returns text")
            .contains("as B would have it"),
        "B's content must not have reached the document"
    );
}

#[tokio::test]
async fn two_assistant_sessions_reordering_blocks_second_writer_gets_doc_rev_conflict() {
    let boot = boot().await;
    seed(&boot).await;

    let (a_read, b_read) = read_both(&boot).await;

    let a_last = blocks(&a_read).last().expect("seeded blocks").clone();
    let b_last = blocks(&b_read).last().expect("seeded blocks").clone();

    // A reorders, consuming the docRev both of them read.
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        assistant_identity(&boot),
        json!({
            "id": a_last["id"],
            "to_index": 0,
            "if_doc_rev": doc_rev(&a_read),
        }),
    )
    .await
    .expect("A's move lands: it is holding the current docRev");
    let after_a = persisted(&boot).await;
    assert_ne!(
        doc_rev(&after_a.read),
        doc_rev(&a_read),
        "a successful move advances docRev — otherwise B's docRev would \
         still be current"
    );

    // B reorders with the docRev it read before A's move.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        assistant_b_identity(&boot),
        json!({
            "id": b_last["id"],
            "to_index": 1,
            "if_doc_rev": doc_rev(&b_read),
        }),
    )
    .await
    .expect_err("B is reordering on top of A's move with a stale docRev");
    // The document-level comparator, naming the docRev B held.
    assert_rev_conflict(
        err,
        "blocks.move",
        &format!(
            "document revision conflict: current doc_rev is {}, expected if_doc_rev {}",
            doc_rev(&after_a.read),
            doc_rev(&b_read),
        ),
    );

    let after_b = persisted(&boot).await;
    assert_untouched(&after_a, &after_b, "blocks.move");
}

#[tokio::test]
async fn two_assistant_sessions_rewriting_the_whole_document_second_writer_gets_doc_rev_conflict() {
    let boot = boot().await;
    seed(&boot).await;

    let (a_read, b_read) = read_both(&boot).await;

    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_identity(&boot),
        json!({
            "body": "# A\n\nalpha, whole-document rewrite by A\n\n# B\n\nbeta\n",
            "if_doc_rev": doc_rev(&a_read),
        }),
    )
    .await
    .expect("A's whole-document write lands: it is holding the current docRev");
    let after_a = persisted(&boot).await;
    assert_ne!(
        doc_rev(&after_a.read),
        doc_rev(&a_read),
        "a successful write_markdown advances docRev"
    );

    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_b_identity(&boot),
        json!({
            "body": "# A\n\nalpha, whole-document rewrite by B\n\n# B\n\nbeta\n",
            "if_doc_rev": doc_rev(&b_read),
        }),
    )
    .await
    .expect_err("B is rewriting the whole document off a stale docRev");
    assert_rev_conflict(
        err,
        "write_markdown",
        &format!(
            "document revision conflict: current doc_rev is {}, expected if_doc_rev {}",
            doc_rev(&after_a.read),
            doc_rev(&b_read),
        ),
    );

    let after_b = persisted(&boot).await;
    assert_untouched(&after_a, &after_b, "write_markdown");
    assert!(
        after_b.read["text"]
            .as_str()
            .expect("read returns text")
            .contains("rewrite by A"),
        "A's whole-document write, not B's, is what survived"
    );
    assert!(
        !after_b.read["text"]
            .as_str()
            .expect("read returns text")
            .contains("rewrite by B"),
        "B's whole-document write must not have reached the document"
    );
}
