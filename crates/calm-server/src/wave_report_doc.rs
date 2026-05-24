//! Issue #247 PR1 — CRDT storage foundation for the wave-report card's
//! `body` + `summary` fields.
//!
//! The kernel stores an opaque `automerge` document blob in
//! `cards.body_crdt` alongside the legacy `payload` JSON column. The
//! JSON column remains the wire format the REST + WS read paths and
//! the frontend consume; this CRDT lives entirely server-side and
//! exists to give future PRs a substrate for:
//!
//!   * tracking a per-card `body_rev` derived from doc state for
//!     optimistic concurrency on user-facing edits,
//!   * letting concurrent edits (spec agent + human) merge cleanly
//!     without one wholesale-clobbering the other,
//!   * surfacing a per-field edit log for "what changed and when."
//!
//! ## Storage layering — no per-field policy in the CRDT layer
//!
//! Both `summary` and `body` are top-level `automerge::Text` objects
//! at the doc root. **The CRDT layer treats them identically** — no
//! "merge body via Myers but clobber summary," no field-specific
//! application logic. That keeps the storage layer pure (opaque bytes
//! in, opaque bytes out, two `Text` slots) and pushes any per-field
//! merge nuance up into a future merge-policy layer if we ever decide
//! we want one.
//!
//! The downside: under concurrent edits, automerge's internal Myers
//! diff inside `update_text` can occasionally produce a less-than-
//! ideal interleaving on the short summary field. The accepted trade
//! is "clean layering > best-effort summary merge"; the per-card edit
//! log a later PR adds will preserve before-states, so a degenerate
//! merge is recoverable rather than data loss.
//!
//! ## Wire-format invariant
//!
//! The frontend never sees CRDT bytes. The payload JSON shape stays
//! identical to v1; only the server-side merge path consults the CRDT.
//! See `wave_report.rs::WaveReportPayload` for the wire contract.
//!
//! ## Round-trip + projection contract
//!
//!   * [`ReportDoc::from_payload`] — seed a fresh doc from current
//!     `(summary, body)` values; used at first-touch backfill of a
//!     wave-report card whose `cards.body_crdt` is still NULL (every
//!     pre-#247 row, plus the lazy-init branch in
//!     `mcp_server::tools::wave_report::persist_report`).
//!   * [`ReportDoc::to_bytes`] — serialize via `AutoCommit::save()`.
//!     The bytes are opaque to SQL and to every other module — they
//!     only travel out of this module to the `body_crdt` column.
//!   * [`ReportDoc::from_bytes`] — inverse of `to_bytes`, via
//!     `AutoCommit::load()`. Returns an error if the blob is corrupt
//!     (a row that fails to load is an invariant violation — pre-PR1
//!     rows are NULL, post-PR1 rows are always written via
//!     `to_bytes`).
//!   * [`ReportDoc::update`] — calls `AutoCommit::update_text` on both
//!     fields uniformly. Identical content is a no-op at the doc
//!     level (automerge collapses zero-diff updates).
//!   * [`ReportDoc::project`] — read current `(summary, body)` text
//!     out of the doc. The values returned here are what the caller
//!     must then write back into the `WaveReportPayload` so the JSON
//!     cache and the CRDT remain bit-for-bit consistent on the read
//!     path.

use anyhow::{Context, Result};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};

use crate::wave_report::WaveReportPayload;

/// Field key for the summary text object at the doc root. Pin as a
/// constant so a typo can't silently drift between the writer side
/// (`put_object`) and the reader side (`get` + `text`).
const FIELD_SUMMARY: &str = "summary";
/// Field key for the body text object at the doc root.
const FIELD_BODY: &str = "body";

/// Opaque CRDT document holding the wave-report's `summary` + `body`.
///
/// Newtype around `automerge::AutoCommit` so the rest of the kernel
/// never imports `automerge` directly. The only entry points are the
/// four methods on this struct — every call site goes through them.
pub struct ReportDoc(AutoCommit);

impl ReportDoc {
    /// Seed a brand-new doc from a payload snapshot. Used at first-
    /// touch of any wave-report card whose `cards.body_crdt` is still
    /// NULL — i.e. every pre-#247 row, plus the lazy-init branch in
    /// `persist_report`.
    ///
    /// The shape created here is the doc-layer invariant **every**
    /// loaded doc must satisfy:
    ///
    /// ```text
    /// ROOT
    ///   ├── summary : Text(<payload.summary>)
    ///   └── body    : Text(<payload.body>)
    /// ```
    ///
    /// Both fields are `Text` regardless of content — empty summary
    /// included. Keeping the shape uniform means [`Self::update`] /
    /// [`Self::project`] never have to branch on "did this row come
    /// from old data."
    pub fn from_payload(payload: &WaveReportPayload) -> Self {
        let mut doc = AutoCommit::new();
        // `put_object` returns the new object id; `update_text` then
        // seeds the text contents. Two-call shape matches the standard
        // automerge usage pattern documented on `Transactable`.
        let summary_id = doc
            .put_object(&ROOT, FIELD_SUMMARY, ObjType::Text)
            .expect("put_object on fresh AutoCommit cannot fail");
        doc.update_text(&summary_id, &payload.summary)
            .expect("update_text on freshly-minted Text obj cannot fail");
        let body_id = doc
            .put_object(&ROOT, FIELD_BODY, ObjType::Text)
            .expect("put_object on fresh AutoCommit cannot fail");
        doc.update_text(&body_id, &payload.body)
            .expect("update_text on freshly-minted Text obj cannot fail");
        Self(doc)
    }

    /// Load a doc from its `to_bytes` serialization. Returns an error
    /// for corrupt blobs; callers map that to an `internal` error
    /// since a row that fails to load is an invariant violation
    /// (pre-PR1 rows are NULL — `body_crdt IS NULL` is the lazy-init
    /// signal, not a load error — and post-PR1 rows always come from
    /// `to_bytes`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let doc = AutoCommit::load(bytes).context("automerge load")?;
        Ok(Self(doc))
    }

    /// Serialize via `AutoCommit::save()`. The bytes are opaque to
    /// every consumer outside this module; the only legal destination
    /// is the `cards.body_crdt` column.
    pub fn to_bytes(&mut self) -> Vec<u8> {
        self.0.save()
    }

    /// Replace both fields' contents via automerge's diff-aware
    /// `update_text` (internally a Myers diff against the current
    /// text — preserves character-level history that a wholesale
    /// `delete + insert` would erase, which is what makes future
    /// concurrent merges meaningful).
    ///
    /// Identical content is a no-op at the doc level — automerge's
    /// diff collapses to zero operations, so `to_bytes` after a
    /// content-equal `update` yields the same logical document
    /// (modulo automerge's internal head-tracking, which the byte
    /// format does not surface to callers).
    pub fn update(&mut self, new_summary: &str, new_body: &str) {
        let summary_id = self
            .text_id(FIELD_SUMMARY)
            .expect("doc invariant: summary Text must exist at root");
        self.0
            .update_text(&summary_id, new_summary)
            .expect("update_text on existing Text obj cannot fail");
        let body_id = self
            .text_id(FIELD_BODY)
            .expect("doc invariant: body Text must exist at root");
        self.0
            .update_text(&body_id, new_body)
            .expect("update_text on existing Text obj cannot fail");
    }

    /// Read the current `(summary, body)` text values out of the
    /// doc. The caller must thread these back into the
    /// `WaveReportPayload` it writes to the `payload` JSON column —
    /// the CRDT is authoritative, the JSON is a cache.
    pub fn project(&self) -> (String, String) {
        let summary_id = self
            .text_id(FIELD_SUMMARY)
            .expect("doc invariant: summary Text must exist at root");
        let body_id = self
            .text_id(FIELD_BODY)
            .expect("doc invariant: body Text must exist at root");
        let summary = self
            .0
            .text(&summary_id)
            .expect("text() on existing Text obj cannot fail");
        let body = self
            .0
            .text(&body_id)
            .expect("text() on existing Text obj cannot fail");
        (summary, body)
    }

    /// Resolve a field's object id off the doc root. Returns `None`
    /// if the field is missing — an invariant violation, since every
    /// doc that exits this module satisfies the shape laid down by
    /// [`Self::from_payload`]. We discard the `Value` half of the
    /// `get` tuple because the next call sites (`update_text` /
    /// `text`) fail loudly if the type isn't `Text`, which is the
    /// right behavior for an invariant break.
    fn text_id(&self, field: &str) -> Option<automerge::ObjId> {
        self.0.get(&ROOT, field).ok().flatten().map(|(_, id)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> WaveReportPayload {
        WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            summary: "spec agent did a thing".to_string(),
            body: "# Goal\n\nReplace the foo with the bar.\n\n# Progress\n\nfoo->bar.\n"
                .to_string(),
        }
    }

    #[test]
    fn from_payload_then_project_returns_original_values() {
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        let (summary, body) = doc.project();
        assert_eq!(summary, payload.summary);
        assert_eq!(body, payload.body);
        // Force a save round-trip too — project before save mustn't
        // depend on any pending-op state that disappears post-save.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s2, b2) = reloaded.project();
        assert_eq!(s2, payload.summary);
        assert_eq!(b2, payload.body);
    }

    #[test]
    fn from_payload_handles_empty_summary() {
        // The "agent hasn't written a summary yet" case — empty string
        // is a valid value (see WaveReportPayload doc); must round-trip
        // identically through the doc layer.
        let payload = WaveReportPayload {
            schema_version: 1,
            summary: String::new(),
            body: "# Goal\n".to_string(),
        };
        let mut doc = ReportDoc::from_payload(&payload);
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s, b) = reloaded.project();
        assert_eq!(s, "");
        assert_eq!(b, "# Goal\n");
    }

    #[test]
    fn update_then_project_returns_new_values() {
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        doc.update("new summary", "# Heading\n\nnew body.\n");
        let (s, b) = doc.project();
        assert_eq!(s, "new summary");
        assert_eq!(b, "# Heading\n\nnew body.\n");
        // And it survives a save round-trip.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s2, b2) = reloaded.project();
        assert_eq!(s2, "new summary");
        assert_eq!(b2, "# Heading\n\nnew body.\n");
    }

    #[test]
    fn identical_update_is_a_noop_at_byte_level() {
        // The point of `update_text`'s internal diff is that re-asserting
        // the same content produces zero ops. We can't peek inside the
        // op log from the public API, but we can verify that two
        // successive saves of identical-content updates produce byte
        // sequences whose decoded content is identical and whose lengths
        // don't grow unboundedly. (Automerge bumps internal heads on
        // each commit boundary, so byte equality isn't guaranteed; we
        // assert the projected text equality + a sane size bound.)
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        let first = doc.to_bytes();
        doc.update(&payload.summary, &payload.body);
        let second = doc.to_bytes();
        // Both saves must decode to the original payload.
        let r1 = ReportDoc::from_bytes(&first).unwrap();
        let r2 = ReportDoc::from_bytes(&second).unwrap();
        assert_eq!(
            r1.project(),
            (payload.summary.clone(), payload.body.clone())
        );
        assert_eq!(r2.project(), (payload.summary, payload.body));
        // No-op update doesn't append a chunk per call — bound the
        // post-update size at <= 2× the pre-update size as a smoke
        // check that we're not silently logging the full text again.
        assert!(
            second.len() <= first.len() * 2,
            "no-op update should not double the doc size: first={}, second={}",
            first.len(),
            second.len()
        );
    }

    #[test]
    fn round_trip_preserves_multibyte_emoji_and_crlf() {
        // Regression pin for the read path: automerge `Text` is
        // logically a sequence of Unicode scalar values, and
        // `update_text`'s Myers diff operates on character boundaries.
        // Verify that the bytes we hand the JSON cache after a
        // `from_payload → to_bytes → from_bytes → project` round-trip
        // are byte-for-byte identical to the input across the awkward
        // cases: multi-byte UTF-8 (Chinese), multi-codepoint emoji
        // (the flag is two regional-indicator codepoints), and CRLF
        // line endings; plus a trailing newline that whitespace-
        // trimming bugs love to eat.
        let summary = "中文测试 🎉 🇨🇳";
        let body = "line1\r\nline2 中文 🎉 🇨🇳\r\n";
        let payload = WaveReportPayload {
            schema_version: 1,
            summary: summary.to_string(),
            body: body.to_string(),
        };

        let mut doc = ReportDoc::from_payload(&payload);
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s, b) = reloaded.project();
        // Byte-for-byte equality; `as_bytes` makes a UTF-8 corruption
        // surface as a length mismatch rather than a `String` PartialEq
        // pretty-print that hides surrogate-pair-style bugs.
        assert_eq!(s.as_bytes(), summary.as_bytes());
        assert_eq!(b.as_bytes(), body.as_bytes());

        // And the update path must preserve them too — re-write with
        // a different multi-byte payload and re-project.
        let mut doc2 = ReportDoc::from_bytes(&bytes).expect("re-load for update");
        let new_summary = "新摘要 🚀 🇯🇵";
        let new_body = "第一行\r\n第二行 🎊\r\n";
        doc2.update(new_summary, new_body);
        let (s2, b2) = doc2.project();
        assert_eq!(s2.as_bytes(), new_summary.as_bytes());
        assert_eq!(b2.as_bytes(), new_body.as_bytes());
        // Survives one more save round-trip on top of the update.
        let bytes2 = doc2.to_bytes();
        let reloaded2 = ReportDoc::from_bytes(&bytes2).expect("post-update round-trip");
        let (s3, b3) = reloaded2.project();
        assert_eq!(s3.as_bytes(), new_summary.as_bytes());
        assert_eq!(b3.as_bytes(), new_body.as_bytes());
    }

    #[test]
    fn concurrent_fork_merge_preserves_both_edits() {
        // The "future merge value" smoke test from the PR1 spec: fork
        // two replicas off the same root, each `update_text`s a
        // disjoint part of `body`, merge them, and both edits must
        // survive. PR1 itself never uses the merge path (the kernel
        // is single-writer), but proving this works pins the
        // foundation we're building toward.
        let payload = WaveReportPayload {
            schema_version: 1,
            summary: "shared".to_string(),
            body: "# A\n\nalpha\n\n# B\n\nbeta\n".to_string(),
        };
        let mut origin = ReportDoc::from_payload(&payload);
        let bytes = origin.to_bytes();

        let mut replica_a = ReportDoc::from_bytes(&bytes).unwrap();
        let mut replica_b = ReportDoc::from_bytes(&bytes).unwrap();

        // Replica A edits the top half (`alpha` → `ALPHA`); replica B
        // edits the bottom (`beta` → `BETA`). Disjoint character
        // ranges so Myers can interleave both edits cleanly.
        let (_, body_a) = replica_a.0.get(&ROOT, FIELD_BODY).unwrap().unwrap();
        replica_a
            .0
            .update_text(&body_a, "# A\n\nALPHA\n\n# B\n\nbeta\n")
            .unwrap();
        let (_, body_b) = replica_b.0.get(&ROOT, FIELD_BODY).unwrap().unwrap();
        replica_b
            .0
            .update_text(&body_b, "# A\n\nalpha\n\n# B\n\nBETA\n")
            .unwrap();

        // Merge B into A. Order-independent for CRDTs; pick one
        // direction for the assertion.
        replica_a.0.merge(&mut replica_b.0).expect("merge replicas");
        let (merged_summary, merged_body) = replica_a.project();
        assert_eq!(merged_summary, "shared", "summary stayed identical");
        assert!(
            merged_body.contains("ALPHA"),
            "replica A's edit survived: body = {merged_body:?}"
        );
        assert!(
            merged_body.contains("BETA"),
            "replica B's edit survived: body = {merged_body:?}"
        );
    }
}
