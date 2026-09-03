//! #1189 S6 — G-C: **two real assistant conversations interleaving on one
//! track's report**.
//!
//! §3.3 rules that concurrency needs no lock because the existing CAS
//! (`if_rev` / `if_doc_rev`, checked inside the persist transaction against
//! the parsed CRDT document) already closes the lost-update window. The
//! entire weight of that ruling rests on a claim nothing in the repo tested:
//! that when **two genuine sessions** read the same revision and then write,
//! the second one is refused.
//!
//! The pre-existing conflict cases (`mcp_track_report_blocks.rs:124-136,
//! 609-641, 799-820`) do not establish that. They are one session handing
//! itself a rev the test author typed in, which proves the comparison
//! rejects a value that does not match — a statement about the comparator,
//! not about interleaving. The difference that matters is **where the rev
//! comes from**: there it is fabricated, here both sessions obtain it from
//! their own `calm.report.read`, which is the only source production has.
//!
//! Each case therefore:
//!
//!   1. has assistant **A** and assistant **B** — two distinct
//!      `CardRole::Assistant` cards, two distinct non-root sessions —
//!      each call `calm.report.read` for itself, and asserts both that the
//!      two identities really are different (`assert_two_distinct_conversations`
//!      — the one property that separates this file from the pre-existing
//!      conflict cases, so it is the one property that must not be assumed)
//!      and that the two reads agree (they are genuinely racing from the
//!      same point);
//!   2. lets A write with what A read → succeeds;
//!   3. has B write with what B read, now stale → **`-32001`**;
//!   4. asserts **B wrote nothing**: the persisted CRDT bytes, the read
//!      projection, and the event log are all identical to the moment A
//!      finished. §7's wording is "the later writer gets a rev conflict
//!      *rather than silently overwriting*"; an error-code-only assertion
//!      cannot rule out "it errored and also wrote".
//!
//! Covered write mouths: `blocks.upsert` (block-level `if_rev`),
//! `blocks.move` (`if_doc_rev`) and `write_markdown` (`if_doc_rev`).
//!
//! ## What "B wrote nothing" does and does not compare
//!
//! Three persistence surfaces are compared byte-for-byte / value-for-value:
//! the card's `body_crdt` blob, the read projection, and the event-log
//! length. Three others are **not**: the tasks projection table, the
//! track-VCS manifest, and the *content* of the events (as opposed to their
//! count).
//!
//! That is sound only because of a property of the current implementation,
//! not of the contract: every one of those writes happens inside the same
//! transaction as the report write, and the CAS runs first inside it, so a
//! conflict rolls all of them back together — comparing them would be
//! comparing the same rollback three more times. **If any of those writes
//! ever moves out of the report-write transaction** (a post-commit hook, a
//! background projector, an outbox drain), that reasoning dies and this
//! file must grow explicit assertions for whichever surface moved: it is
//! the only thing here that would not notice.
//!
//! Deliberately **not** here: any "you may not edit someone else's block"
//! semantics. §3.5 rules that out of scope — CAS guarantees no lost update,
//! not the absence of a fight, and a lock would not fix it either.

#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, assistant_b_identity, assistant_identity, boot, call_tool, spec_identity,
};
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::track_report_blocks::{
    RPC_REV_CONFLICT, TOOL_REPORT_BLOCKS_MOVE, TOOL_REPORT_BLOCKS_UPSERT,
    TOOL_REPORT_WRITE_MARKDOWN,
};
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "calm.report.read";

/// What one session sees when it reads the report for itself. `with_markers`
/// is on so the block identities are inside the text too.
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

/// Everything a write could possibly have disturbed: the stored CRDT bytes
/// (the actual document, byte for byte), the projection a reader gets back,
/// and the length of the persisted event log.
///
/// Comparing the raw `body_crdt` blob is the load-bearing part. The read
/// projection could in principle round-trip a change away; the bytes cannot.
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

/// The §7 assertion proper: after the refused write, the document is what A
/// left behind, byte for byte, and nothing was logged.
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

/// Two H1 sections, written by the spec so both assistants start from a
/// plain prose document (no task fences, so the P2 guard never enters the
/// picture and a conflict is the only thing under test).
async fn seed(boot: &Boot) {
    call_tool(
        boot,
        TOOL_REPORT_WRITE,
        spec_identity(boot),
        json!({
            "body": "# A\n\nalpha\n\n# B\n\nbeta\n",
            "summary": "seeded",
            "message": "seed",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("spec seeds the report");
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

/// **The distinguishing property of this whole file.** Every pre-existing
/// conflict case is one session handing itself a stale rev; the only thing
/// these three cases add is that the two writers are *two conversations*.
/// Nothing else below would notice if that stopped being true: were
/// `assistant_b_identity` to start returning A's card and A's session, all
/// three cases would still pass — one session re-using an invalidated rev
/// conflicts in exactly the same way, with the same `-32001` and the same
/// "nothing was written" — and the file would have silently decayed back
/// into the shape it exists to improve on. So the two identities are
/// asserted distinct, on both axes the recorder gate resolves (card → role
/// and track; session → card), before either of them writes.
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

/// Both sessions read for themselves and are proven to be (a) genuinely two
/// conversations and (b) starting from the same revision.
async fn read_both(boot: &Boot) -> (Value, Value) {
    let a = assistant_identity(boot);
    let b = assistant_b_identity(boot);
    assert_two_distinct_conversations(&a, &b);
    let a_read = read_as(boot, a).await;
    let b_read = read_as(boot, b).await;
    assert_same_starting_point(&a_read, &b_read);
    (a_read, b_read)
}

/// -32001 is shared by the block-level (`if_rev`) and document-level
/// (`if_doc_rev`) comparators, so the code alone does not say *which* CAS
/// refused the write — a mutation that made the wrong one fire would still
/// look green. `detail` is the fragment of the refusal that only one of them
/// can produce, including the exact stale rev B was holding.
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

// ---------------------------------------------------------------------------
// blocks.upsert — block-level `if_rev`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_assistant_sessions_replacing_one_block_second_writer_gets_rev_conflict() {
    let boot = boot().await;
    seed(&boot).await;

    // 1. Both sessions read for themselves. Neither rev below is written by
    //    this test — they come out of the tool.
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
    // The block-level comparator, naming B's block and the stale rev it held
    // — not the document-level one, which shares the -32001 code.
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

// ---------------------------------------------------------------------------
// blocks.move — document-level `if_doc_rev`
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// write_markdown — whole-document `if_doc_rev`
// ---------------------------------------------------------------------------

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
