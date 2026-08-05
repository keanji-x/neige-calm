//! Issue #247 PR1 / #960 PR2 — CRDT storage for the wave-report card.
//!
//! The kernel stores an opaque `automerge` document blob in
//! `cards.body_crdt` alongside the legacy `payload` JSON column. The
//! JSON column remains the wire format the REST + WS read paths and
//! the frontend consume; this CRDT lives entirely server-side.
//!
//! ## Document layout (v2, #960 PR2)
//!
//! ```text
//! ROOT
//!   ├── summary : Text(<payload.summary>)
//!   ├── blocks  : Map<block_id, Map { kind: Str, rev: Uint, text: Text }>
//!   └── order   : List<Str(block_id)>
//! ```
//!
//! The block map is the **authoritative source** for the report body.
//! `body` no longer exists at the doc root — it is a pure projection:
//! [`ReportDoc::project`] concatenates each block's `text` in `order`
//! order, which by the `flatten(blocks) == body` invariant of
//! `calm_types::report_blocks` reproduces the flat markdown byte for
//! byte. Prose markdown is stored as `Text` so character-level edit
//! history (and future concurrent merges) stay meaningful.
//!
//! #960 PR3 — a non-prose block's `text` holds its **canonical
//! `neige-block` fence** (`calm_types::report_blocks::fence`), i.e.
//! exactly the bytes it contributes to the flat projection; the JSON
//! payload the wire mirrors is recovered by parsing that fence
//! ([`Self::blocks_snapshot`]). Storing the projection bytes keeps
//! `project()` a plain per-block concatenation for every kind.
//!
//! ## Legacy layout + lazy migration
//!
//! Docs written before #960 PR2 have `ROOT.body: Text` and no
//! `blocks` key. [`ReportDoc::from_bytes`] stays a pure load;
//! [`ReportDoc::ensure_blocks_layout`] detects the old shape (O(1):
//! `blocks` key absent) and rebuilds it in place — project the old
//! body, split it into slices, reuse the block ids the caller passes
//! from the payload JSON's PR1-derived `blocks` cache (minting fresh
//! `b_xxxx` ids where there is no match), then delete `ROOT.body`.
//! The persist boundary (`wave_report::persist_report_with_shadow`)
//! calls it right after loading, inside the same transaction, so the
//! migrated bytes are written back atomically with the payload.
//!
//! [`ReportDoc::project`] tolerates a not-yet-migrated doc (read-only
//! fallback to `ROOT.body`); every mutating entry point requires the
//! v2 layout.
//!
//! ## Wire-format invariant
//!
//! The frontend never sees CRDT bytes. The payload JSON (`summary`,
//! `body`, `blocks`) is a projection cache the persist boundary
//! rewrites from this doc on every write.

use anyhow::{Context, Result, bail, ensure};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use serde_json::json;
use std::collections::{HashMap, HashSet};

use calm_types::report_blocks::{
    BlockSlice, KIND_PROSE, flat_text, mint_id, parse_fence, reassign_ids, reassign_ids_with_hints,
    split_body,
};

use crate::wave_report::{ReportBlock, WaveReportPayload};

/// Field key for the summary text object at the doc root.
const FIELD_SUMMARY: &str = "summary";
/// Field key for the block map at the doc root (v2 layout).
const FIELD_BLOCKS: &str = "blocks";
/// Field key for the block-id order list at the doc root (v2 layout).
const FIELD_ORDER: &str = "order";
/// Document-wide optimistic-concurrency revision (Uint). Legacy docs
/// omit it and therefore read as revision zero until their first write.
const FIELD_DOC_REV: &str = "doc_rev";
/// Field key for the legacy (pre-#960) body text object. Only the
/// migrator and the read-only projection fallback may touch it.
const LEGACY_FIELD_BODY: &str = "body";
/// Block-entry key: block kind (`prose` or a data kind, #960 PR3).
const KEY_KIND: &str = "kind";
/// Block-entry key: per-block optimistic-concurrency revision (Uint).
const KEY_REV: &str = "rev";
/// Block-entry key: the block's flat content (Text) — markdown for
/// prose, the canonical `neige-block` fence for non-prose kinds.
const KEY_TEXT: &str = "text";

/// Opaque CRDT document holding the wave-report's `summary` + block map.
///
/// Newtype around `automerge::AutoCommit` so the rest of the kernel
/// never imports `automerge` directly. Every call site goes through
/// the methods on this struct.
pub struct ReportDoc(AutoCommit);

impl ReportDoc {
    /// Seed a brand-new doc from a payload snapshot. Used at first-
    /// touch of any wave-report card whose `cards.body_crdt` is still
    /// NULL — i.e. every pre-#247 row, plus the lazy-init branch in
    /// `persist_report`.
    ///
    /// The body is split into slices and aligned against the payload's
    /// `blocks` cache (if present), so PR1-derived block ids survive
    /// the seed instead of being re-minted.
    pub fn from_payload(payload: &WaveReportPayload) -> Self {
        let mut doc = AutoCommit::new();
        let summary_id = doc
            .put_object(&ROOT, FIELD_SUMMARY, ObjType::Text)
            .expect("put_object on fresh AutoCommit cannot fail");
        doc.update_text(&summary_id, &payload.summary)
            .expect("update_text on freshly-minted Text obj cannot fail");
        let blocks = reassign_ids(
            payload.blocks.as_deref().unwrap_or_default(),
            &split_body(&payload.body),
        );
        Self::write_blocks_layout(&mut doc, &blocks);
        Self(doc)
    }

    /// Seed a report doc from an already-authoritative ordered block snapshot.
    pub fn from_blocks_exact(summary: &str, blocks: &[ReportBlock]) -> Result<Self> {
        let mut seen = HashSet::new();
        for block in blocks {
            ensure!(
                seen.insert(block.id.as_str()),
                "duplicate block id {} in exact report snapshot",
                block.id
            );
        }

        let mut doc = AutoCommit::new();
        let summary_id = doc
            .put_object(&ROOT, FIELD_SUMMARY, ObjType::Text)
            .context("create exact report summary")?;
        doc.update_text(&summary_id, summary)
            .context("write exact report summary")?;
        // Deliberately do not route this authoritative snapshot through
        // `write_blocks_layout`: that defensive seeding helper may remint a
        // duplicate id. The uniqueness check above is the fork boundary, and
        // every id below is written byte-for-byte as supplied.
        let blocks_id = doc
            .put_object(&ROOT, FIELD_BLOCKS, ObjType::Map)
            .context("create exact report blocks map")?;
        let order_id = doc
            .put_object(&ROOT, FIELD_ORDER, ObjType::List)
            .context("create exact report order list")?;
        for (index, block) in blocks.iter().enumerate() {
            Self::insert_block_entry(
                &mut doc,
                &blocks_id,
                &block.id,
                &block.kind,
                block.rev,
                &flat_text(block),
            );
            doc.insert(&order_id, index, block.id.as_str())
                .context("write exact report order entry")?;
        }
        Ok(Self(doc))
    }

    /// Read the authoritative document revision from the CRDT root.
    /// Missing (legacy) fields are revision zero.
    pub fn doc_rev(&self) -> Result<u64> {
        let Some((value, _)) = self.0.get(&ROOT, FIELD_DOC_REV).context("read doc_rev")? else {
            return Ok(0);
        };
        match value {
            Value::Scalar(value) => value
                .to_u64()
                .context("doc_rev must be an unsigned integer"),
            Value::Object(_) => bail!("doc_rev must be a scalar"),
        }
    }

    /// Increment `doc_rev` after a successful mutation. Called inside
    /// the persist transaction so the revision and report bytes commit
    /// atomically. This root scalar is a last-writer-wins register, so
    /// callers must serialize mutations through the persist transaction;
    /// concurrently merged branches could otherwise both publish N+1 and
    /// make a stale N+1 anchor appear current. Overflow is treated as
    /// corrupted/exhausted state.
    pub fn increment_doc_rev(&mut self) -> Result<u64> {
        let next = self.doc_rev()?.checked_add(1).context("doc_rev overflow")?;
        self.0
            .put(&ROOT, FIELD_DOC_REV, next)
            .context("write doc_rev")?;
        Ok(next)
    }

    /// Load a doc from its `to_bytes` serialization. Pure load — no
    /// migration happens here; callers that intend to mutate a doc
    /// must run [`Self::ensure_blocks_layout`] first. Returns an error
    /// for corrupt blobs; callers map that to an `internal` error
    /// since a row that fails to load is an invariant violation.
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

    /// Deterministic, opaque encoding of the doc's Automerge canonical
    /// heads (#955 §5.2) — the `base_doc_heads` anchor `neige.report.get`
    /// hands to proposing plugins and the accept transaction compares
    /// against. Change hashes are content-derived, so the token is
    /// stable across process restarts and save/load round-trips; ANY
    /// committed change (from any actor) yields a different token.
    ///
    /// Encoding: sort the head hashes (hex), hash the sorted sequence
    /// with SHA-256, and prefix with a scheme tag so a future encoding
    /// change is detectable rather than silently colliding. Sorting
    /// makes the token independent of automerge's head ordering; the
    /// second-stage hash keeps it fixed-size no matter how many
    /// concurrent heads exist. Consumers MUST treat it as opaque —
    /// equality is the only defined operation.
    ///
    /// `&mut self` because `get_heads` (like `save`) commits any
    /// pending transaction before reading — call order next to
    /// `to_bytes` is therefore irrelevant.
    pub fn doc_heads(&mut self) -> String {
        use sha2::{Digest, Sha256};
        let mut heads: Vec<String> = self.0.get_heads().iter().map(|h| h.to_string()).collect();
        heads.sort();
        let mut hasher = Sha256::new();
        for head in &heads {
            hasher.update(head.as_bytes());
            // Unambiguous separator: hex never contains NUL.
            hasher.update([0u8]);
        }
        format!("ah1:{:x}", hasher.finalize())
    }

    /// Lazily migrate a legacy (pre-#960) doc to the v2 block layout.
    ///
    /// Returns `Ok(false)` when the doc already has the `blocks` map
    /// (O(1) check, the common case). Otherwise: read the legacy
    /// `ROOT.body` text, split it, align the slices against
    /// `hint_blocks` (the payload JSON's PR1-derived `blocks` cache,
    /// so best-effort ids become durable ones), write the
    /// `blocks`/`order` layout, delete `ROOT.body`, and return
    /// `Ok(true)`.
    pub fn ensure_blocks_layout(&mut self, hint_blocks: Option<&[ReportBlock]>) -> Result<bool> {
        if self.blocks_map().context("probe blocks map")?.is_some() {
            return Ok(false);
        }
        let (_, body_id) = self
            .0
            .get(&ROOT, LEGACY_FIELD_BODY)
            .context("probe legacy body")?
            .context("legacy doc missing both blocks map and body Text")?;
        let body = self.0.text(&body_id).context("read legacy body text")?;
        let blocks = reassign_ids(hint_blocks.unwrap_or_default(), &split_body(&body));
        Self::write_blocks_layout(&mut self.0, &blocks);
        self.0
            .delete(&ROOT, LEGACY_FIELD_BODY)
            .context("delete legacy body")?;
        Ok(true)
    }

    /// Wholesale replace: the compatibility shim behind the legacy
    /// `calm.report.write`/`edit` tools and the REST user-edit path.
    ///
    /// Splits `new_body`, aligns the slices against the current block
    /// map via `calm_types::report_blocks::reassign_ids`, and lands
    /// the result at block granularity: changed blocks get a
    /// a fresh linear-write Text child + `rev + 1`, new blocks
    /// get a fresh map entry (`rev = 1`), vanished blocks are deleted,
    /// and `order` is rewritten when it changed. Byte-identical
    /// content is a doc-level no-op (revs untouched, zero text ops).
    ///
    /// Returns an error when the stored doc violates the layout
    /// invariants (malformed CRDT bytes) — never panics.
    pub fn update(&mut self, new_summary: &str, new_body: &str) -> Result<()> {
        let summary_id = self.summary_text_id()?;
        self.0
            .update_text(&summary_id, new_summary)
            .context("update summary text")?;

        let current = self.blocks_snapshot()?;
        let aligned = reassign_ids(&current, &split_body(new_body));
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// Marker-aware wholesale replace behind `calm.report.write_markdown`
    /// (#960 PR2). Same landing semantics as [`Self::update`], but the
    /// caller supplies pre-split slices plus per-slice id hints
    /// (recovered from stripped `<!-- neige:b_xxxx -->` marker lines by
    /// `calm_types::report_blocks::strip_markers_and_split`); hinted
    /// slices bind to their old block exactly, the rest fall back to
    /// the LCS/similarity alignment.
    pub fn update_with_hints(
        &mut self,
        new_summary: &str,
        slices: &[BlockSlice],
        hints: &[Option<String>],
    ) -> Result<()> {
        let summary_id = self.summary_text_id()?;
        self.0
            .update_text(&summary_id, new_summary)
            .context("update summary text")?;

        let current = self.blocks_snapshot()?;
        let aligned = reassign_ids_with_hints(&current, slices, hints);
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// Read the current `(summary, body)` projection out of the doc,
    /// where `body` is the in-`order` concatenation of every block's
    /// text (`flatten` semantics — byte-identical to the flat
    /// markdown the blocks were split from). The caller must thread
    /// these back into the `WaveReportPayload` it writes to the
    /// `payload` JSON column — the CRDT is authoritative, the JSON is
    /// a cache.
    ///
    /// Read-only fallback: a legacy doc that has not been migrated
    /// yet projects its `ROOT.body` text unchanged.
    ///
    /// Returns an error (never panics) when the stored doc violates
    /// the layout invariants — a malformed blob must surface as an
    /// `Internal` error at the persist/read boundary, not crash the
    /// server.
    pub fn project(&self) -> Result<(String, String)> {
        let summary = self.text_at(&ROOT, FIELD_SUMMARY)?;
        let body = if let Some(blocks_id) = self.blocks_map()? {
            let mut body = String::new();
            for id in self.order_ids()? {
                let entry = self.entry_at(&blocks_id, &id)?.with_context(|| {
                    format!("malformed report doc: order id {id} has no blocks entry")
                })?;
                body.push_str(
                    &self
                        .text_at(&entry, KEY_TEXT)
                        .with_context(|| format!("malformed report doc: block {id} text field"))?,
                );
            }
            body
        } else {
            self.text_at(&ROOT, LEGACY_FIELD_BODY)
                .context("malformed report doc: legacy doc must have a body Text at root")?
        };
        Ok((summary, body))
    }

    /// `Ok(true)` when the doc carries a **well-formed** v2
    /// `blocks`/`order` layout: `blocks` is a Map, `order` exists and
    /// is a List, and every order entry resolves to a shape-correct
    /// block (`kind` Str / `rev` Uint / `text` Text). `Ok(false)` only
    /// for the legal legacy shape (no `blocks` at ROOT — pre-#960,
    /// handled by the lazy migrator). Anything in between is
    /// corruption and errors — a damaged v2 doc must never be read as
    /// a valid empty report (#960 PR2 review round 2).
    pub fn has_blocks_layout(&self) -> Result<bool> {
        match self.blocks_map()? {
            None => Ok(false),
            Some(_) => {
                // Full-shape walk; discard the snapshot, keep the
                // validation.
                self.blocks_snapshot()?;
                Ok(true)
            }
        }
    }

    /// Full typed snapshot of the block map in `order` order. The
    /// persist boundary mirrors this into `WaveReportPayload::blocks`.
    /// Prose blocks carry `{ markdown }`; a non-prose block's payload
    /// is parsed back out of its stored canonical fence (#960 PR3) —
    /// a non-prose `text` that is not a well-formed fence of the
    /// stored kind is corruption and errors.
    pub fn blocks_snapshot(&self) -> Result<Vec<ReportBlock>> {
        let Some(blocks_id) = self.blocks_map()? else {
            return Ok(Vec::new());
        };
        // 1:1 layout validation (#960 PR2 review round 3): `order`
        // must be duplicate-free and cover the blocks map exactly —
        // a duplicated order id would project the same block twice,
        // a hidden map entry outside `order` is unreachable state.
        let order = self.order_ids()?;
        let mut seen: HashSet<&str> = HashSet::new();
        for id in &order {
            ensure!(
                seen.insert(id.as_str()),
                "malformed report doc: duplicate id {id} in order"
            );
        }
        let map_len = self.0.keys(&blocks_id).count();
        ensure!(
            map_len == order.len(),
            "malformed report doc: blocks map has {map_len} entries but order lists {}",
            order.len()
        );
        let mut blocks = Vec::new();
        for id in order {
            let entry = self.entry_at(&blocks_id, &id)?.with_context(|| {
                format!("malformed report doc: order id {id} has no blocks entry")
            })?;
            let kind = self
                .0
                .get(&entry, KEY_KIND)
                .with_context(|| format!("read block {id} kind"))?
                .and_then(|(value, _)| value.to_str().map(str::to_string))
                .with_context(|| format!("malformed report doc: block {id} has no Str kind"))?;
            let rev = self
                .0
                .get(&entry, KEY_REV)
                .with_context(|| format!("read block {id} rev"))?
                .and_then(|(value, _)| value.to_u64())
                .with_context(|| format!("malformed report doc: block {id} has no Uint rev"))?;
            let rev = u32::try_from(rev).with_context(|| {
                format!("malformed report doc: block {id} rev {rev} exceeds u32")
            })?;
            let text = self
                .text_at(&entry, KEY_TEXT)
                .with_context(|| format!("malformed report doc: block {id} text field"))?;
            let payload = if kind == KIND_PROSE {
                json!({ "markdown": text })
            } else {
                let fence = parse_fence(&text).with_context(|| {
                    format!(
                        "malformed report doc: block {id} (kind {kind}) text is not a \
                         well-formed neige-block fence"
                    )
                })?;
                ensure!(
                    fence.kind == kind,
                    "malformed report doc: block {id} kind {kind} does not match its \
                     fence kind {}",
                    fence.kind
                );
                fence.payload
            };
            blocks.push(ReportBlock {
                id,
                kind,
                rev,
                payload,
            });
        }
        Ok(blocks)
    }

    /// `(id, kind, rev)` per block, in `order` order — the index the
    /// MCP tool surface (next slice) returns alongside the flat text.
    pub fn block_index(&self) -> Result<Vec<(String, String, u32)>> {
        Ok(self
            .blocks_snapshot()?
            .into_iter()
            .map(|block| (block.id, block.kind, block.rev))
            .collect())
    }

    /// Current rev of a block. `Ok(None)` when the id doesn't exist;
    /// `Err` when the doc or the entry is malformed — rev corruption
    /// must surface as an Internal-level error, never be folded into
    /// "block not found" (which callers map to BadRequest). The
    /// `if_rev` optimistic-concurrency check reads this.
    pub fn block_rev(&self, id: &str) -> Result<Option<u32>> {
        let blocks_id = self
            .blocks_map()?
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        let Some(entry) = self.entry_at(&blocks_id, id)? else {
            return Ok(None);
        };
        let rev = self
            .0
            .get(&entry, KEY_REV)
            .with_context(|| format!("read block {id} rev"))?
            .and_then(|(value, _)| value.to_u64())
            .with_context(|| format!("malformed report doc: block {id} has no Uint rev"))?;
        let rev = u32::try_from(rev)
            .with_context(|| format!("malformed report doc: block {id} rev {rev} exceeds u32"))?;
        Ok(Some(rev))
    }

    /// Insert or replace a single block.
    ///
    ///   * `id = None` — mint a fresh `b_xxxx` id (same style as
    ///     `calm_types::report_blocks::mint_id`), create the block at
    ///     the end of `order` with `rev = 1`, return `(id, 1)`.
    ///   * `id = Some(_)` — replace that block's kind + content and
    ///     bump `rev` by 1. Byte-identical content (same kind, same
    ///     text) is an idempotent no-op: nothing is written and the
    ///     **current** rev is returned, so a retried request cannot
    ///     silently invalidate the caller's `if_rev` anchor (#960 PR2
    ///     review). Unknown id is an error.
    pub fn upsert_block(
        &mut self,
        id: Option<&str>,
        kind: &str,
        content: &str,
    ) -> Result<(String, u32)> {
        // #960 PR3 invariant: a non-prose block's stored text IS its
        // canonical fence. The tool layer renders it; this check keeps
        // a future caller from storing a fence the snapshot cannot
        // parse back.
        if kind != KIND_PROSE {
            let fence = parse_fence(content).with_context(|| {
                format!(
                    "doc invariant: non-prose block content must be a canonical \
                     neige-block fence (kind {kind})"
                )
            })?;
            ensure!(
                fence.kind == kind,
                "doc invariant: fence kind {} does not match block kind {kind}",
                fence.kind
            );
        }
        let blocks_id = self
            .blocks_map()?
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        match id {
            Some(id) => {
                let entry = self
                    .entry_at(&blocks_id, id)?
                    .with_context(|| format!("block {id} not found"))?;
                let rev = self
                    .0
                    .get(&entry, KEY_REV)
                    .context("read block rev")?
                    .and_then(|(value, _)| value.to_u64())
                    .context("doc invariant: block entry has a Uint rev")?;
                let rev = u32::try_from(rev).with_context(|| {
                    format!("malformed report doc: block {id} rev {rev} exceeds u32")
                })?;
                let text_id = self
                    .typed_at(&entry, KEY_TEXT, ObjType::Text)?
                    .with_context(|| format!("malformed report doc: block {id} text field"))?;
                let existing_kind = self
                    .0
                    .get(&entry, KEY_KIND)
                    .context("read block kind")?
                    .and_then(|(value, _)| value.to_str().map(str::to_string));
                let existing_text = self.0.text(&text_id).context("read block text")?;
                if existing_kind.as_deref() == Some(kind) && existing_text == content {
                    // Idempotent replace: byte-identical content moves
                    // nothing — no text op, no rev bump. The persist
                    // boundary still runs (and still emits the dual-
                    // event pair) so the uniform "every persist → two
                    // events" invariant holds.
                    return Ok((id.to_string(), rev));
                }
                let next_rev = rev.saturating_add(1);
                self.0.put(&entry, KEY_KIND, kind).context("put kind")?;
                self.0
                    .put(&entry, KEY_REV, u64::from(next_rev))
                    .context("put rev")?;
                replace_text_object(&mut self.0, &entry, content).context("replace block text")?;
                Ok((id.to_string(), next_rev))
            }
            None => {
                let order_id = self.order_list()?;
                let mut used: HashSet<String> = self.0.keys(&blocks_id).collect();
                let index = self.0.length(&order_id);
                let id = mint_id(content, index, &mut used);
                Self::insert_block_entry(&mut self.0, &blocks_id, &id, kind, 1, content);
                self.0
                    .insert(&order_id, index, id.as_str())
                    .context("append block id to order")?;
                Ok((id, 1))
            }
        }
    }

    /// Move a block to `to_index` (its final index in the unchanged-
    /// length list). Automerge lists have no move op, so this is a
    /// delete + insert on `order`; the block entry itself is
    /// untouched (rev unchanged — ordering is not content).
    pub fn move_block(&mut self, id: &str, to_index: usize) -> Result<()> {
        let order_id = self.order_list()?;
        let ids = self.order_ids()?;
        let from = ids
            .iter()
            .position(|existing| existing == id)
            .with_context(|| format!("block {id} not found"))?;
        ensure!(
            to_index < ids.len(),
            "move_block: index {to_index} out of range (len {})",
            ids.len()
        );
        if from == to_index {
            return Ok(());
        }
        self.0
            .delete(&order_id, from)
            .context("remove from order")?;
        self.0
            .insert(&order_id, to_index, id)
            .context("re-insert into order")?;
        Ok(())
    }

    /// Delete a block: remove its `order` entry and its map entry.
    /// Unknown id is an error.
    pub fn delete_block(&mut self, id: &str) -> Result<()> {
        let blocks_id = self
            .blocks_map()?
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        let order_id = self.order_list()?;
        let index = self
            .order_ids()?
            .iter()
            .position(|existing| existing == id)
            .with_context(|| format!("block {id} not found"))?;
        self.0
            .delete(&order_id, index)
            .context("remove from order")?;
        self.0
            .delete(&blocks_id, id)
            .context("remove block entry")?;
        Ok(())
    }

    // -- internals ---------------------------------------------------

    /// Land an aligned block list produced by `reassign_ids` onto the
    /// doc at block granularity. Content is each block's flat text
    /// (markdown for prose, canonical fence for non-prose — #960 PR3).
    /// The wholesale `Replace` path additionally refuses to stomp
    /// non-prose blocks *before* alignment lands (the guard lives in
    /// `wave_report::apply_report_op`, inside the persist tx).
    fn apply_aligned_blocks(
        &mut self,
        current: &[ReportBlock],
        aligned: &[ReportBlock],
    ) -> Result<()> {
        let blocks_id = self
            .blocks_map()?
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        let keep: HashSet<&str> = aligned.iter().map(|block| block.id.as_str()).collect();
        for old in current {
            if !keep.contains(old.id.as_str()) {
                self.0
                    .delete(&blocks_id, old.id.as_str())
                    .context("delete vanished block entry")?;
            }
        }
        let old_by_id: HashMap<&str, &ReportBlock> = current
            .iter()
            .map(|block| (block.id.as_str(), block))
            .collect();
        for block in aligned {
            let content = flat_text(block);
            match old_by_id.get(block.id.as_str()) {
                Some(old) => {
                    let entry = self
                        .entry_at(&blocks_id, &block.id)?
                        .context("doc invariant: surviving block entry exists")?;
                    if old.kind != block.kind {
                        self.0
                            .put(&entry, KEY_KIND, block.kind.as_str())
                            .context("put block kind")?;
                    }
                    if old.rev != block.rev {
                        self.0
                            .put(&entry, KEY_REV, u64::from(block.rev))
                            .context("put block rev")?;
                    }
                    if flat_text(old) != content {
                        self.typed_at(&entry, KEY_TEXT, ObjType::Text)?
                            .context("doc invariant: block entry has a text field")?;
                        replace_text_object(&mut self.0, &entry, &content)
                            .context("replace block text")?;
                    }
                }
                None => {
                    Self::insert_block_entry(
                        &mut self.0,
                        &blocks_id,
                        &block.id,
                        &block.kind,
                        block.rev,
                        &content,
                    );
                }
            }
        }
        let new_order: Vec<&str> = aligned.iter().map(|block| block.id.as_str()).collect();
        if self.order_ids()? != new_order {
            let order_id = self.order_list()?;
            while self.0.length(&order_id) > 0 {
                self.0
                    .delete(&order_id, 0_usize)
                    .context("clear order list")?;
            }
            for (index, id) in new_order.iter().enumerate() {
                self.0
                    .insert(&order_id, index, *id)
                    .context("rebuild order list")?;
            }
        }
        Ok(())
    }

    /// Create the `blocks` map + `order` list from scratch and fill
    /// them from an aligned block list. Seeding path shared by
    /// `from_payload` and the lazy migrator.
    ///
    /// Duplicate ids in `blocks` are deduplicated defensively: the
    /// first occurrence keeps the id, later occurrences get a freshly
    /// minted one (a duplicate map key would silently overwrite the
    /// first block's entry while `order` still listed the id twice —
    /// projecting the same content twice and breaking the byte-exact
    /// `flatten(blocks) == body` invariant). `reassign_ids*` already
    /// guarantees unique output ids; this guards direct callers and
    /// future refactors.
    fn write_blocks_layout(doc: &mut AutoCommit, blocks: &[ReportBlock]) {
        let blocks_id = doc
            .put_object(&ROOT, FIELD_BLOCKS, ObjType::Map)
            .expect("put_object at root cannot fail");
        let order_id = doc
            .put_object(&ROOT, FIELD_ORDER, ObjType::List)
            .expect("put_object at root cannot fail");
        let mut used: HashSet<String> = blocks.iter().map(|block| block.id.clone()).collect();
        let mut seen: HashSet<String> = HashSet::new();
        for (index, block) in blocks.iter().enumerate() {
            let content = flat_text(block);
            let id = if seen.insert(block.id.clone()) {
                block.id.clone()
            } else {
                // Duplicate id: first occupant wins, mint a fresh one.
                let minted = mint_id(&content, index, &mut used);
                seen.insert(minted.clone());
                minted
            };
            Self::insert_block_entry(doc, &blocks_id, &id, &block.kind, block.rev, &content);
            doc.insert(&order_id, index, id.as_str())
                .expect("insert at list tail cannot fail");
        }
        // Post-condition: `order` never carries a duplicate id.
        debug_assert_eq!(
            seen.len(),
            blocks.len(),
            "write_blocks_layout: order must be duplicate-free"
        );
    }

    /// Create one block entry (`Map { kind, rev, text }`) under the
    /// blocks map. Does not touch `order`.
    fn insert_block_entry(
        doc: &mut AutoCommit,
        blocks_id: &automerge::ObjId,
        id: &str,
        kind: &str,
        rev: u32,
        content: &str,
    ) {
        let entry = doc
            .put_object(blocks_id, id, ObjType::Map)
            .expect("put_object on blocks map cannot fail");
        doc.put(&entry, KEY_KIND, kind)
            .expect("put on fresh map cannot fail");
        doc.put(&entry, KEY_REV, u64::from(rev))
            .expect("put on fresh map cannot fail");
        let text_id = doc
            .put_object(&entry, KEY_TEXT, ObjType::Text)
            .expect("put_object on fresh map cannot fail");
        doc.update_text(&text_id, content)
            .expect("update_text on freshly-minted Text obj cannot fail");
    }

    /// The block-id order, materialized as owned strings. Only legal
    /// in a v2 context (blocks map present): a missing or non-List
    /// `order` is corruption, never "empty" — errors on that and on
    /// non-Str entries.
    fn order_ids(&self) -> Result<Vec<String>> {
        let order_id = self.order_list()?;
        (0..self.0.length(&order_id))
            .map(|index| {
                self.0
                    .get(&order_id, index)
                    .with_context(|| format!("read order entry {index}"))?
                    .and_then(|(value, _)| value.to_str().map(str::to_string))
                    .with_context(|| {
                        format!("malformed report doc: order entry {index} is not a Str block id")
                    })
            })
            .collect()
    }

    /// The summary `Text` object id, validated. Errors (never panics)
    /// when the doc has no summary or it is not a `Text` object.
    fn summary_text_id(&self) -> Result<automerge::ObjId> {
        let (value, id) = self
            .0
            .get(&ROOT, FIELD_SUMMARY)
            .context("read summary")?
            .context("malformed report doc: missing summary at root")?;
        ensure!(
            matches!(value, Value::Object(ObjType::Text)),
            "malformed report doc: summary is not a Text object"
        );
        Ok(id)
    }

    /// Read a validated `Text` object's content at `parent[prop]`.
    /// Errors when the key is absent or holds anything but a `Text`
    /// object (a malformed doc must never panic the read path).
    fn text_at(&self, parent: &automerge::ObjId, prop: &str) -> Result<String> {
        let (value, id) = self
            .0
            .get(parent, prop)
            .with_context(|| format!("read `{prop}`"))?
            .with_context(|| format!("malformed report doc: missing `{prop}`"))?;
        if !matches!(value, Value::Object(ObjType::Text)) {
            bail!("malformed report doc: `{prop}` is not a Text object");
        }
        self.0
            .text(&id)
            .with_context(|| format!("read `{prop}` text"))
    }

    /// Typed child-object lookup: `Ok(None)` when `prop` is absent,
    /// `Err` when the lookup itself fails or the value is present but
    /// not an object of type `ty`. A malformed doc must never be
    /// silently reinterpreted (e.g. a scalar `order` read as "no
    /// order" → empty report) — type errors are corruption, and
    /// corruption surfaces as an error at the persist/read boundary.
    fn typed_at(
        &self,
        parent: &automerge::ObjId,
        prop: &str,
        ty: ObjType,
    ) -> Result<Option<automerge::ObjId>> {
        match self
            .0
            .get(parent, prop)
            .with_context(|| format!("read `{prop}`"))?
        {
            None => Ok(None),
            Some((value, id)) => {
                ensure!(
                    matches!(value, Value::Object(actual) if actual == ty),
                    "malformed report doc: `{prop}` is not a {ty:?} object"
                );
                Ok(Some(id))
            }
        }
    }

    /// The v2 `blocks` map id, or `None` for a legacy (pre-#960) doc.
    /// Errors when `blocks` exists but is not a Map.
    fn blocks_map(&self) -> Result<Option<automerge::ObjId>> {
        self.typed_at(&ROOT, FIELD_BLOCKS, ObjType::Map)
    }

    /// The v2 `order` list id. Every caller is in a v2 context (the
    /// blocks map exists or is required), so "blocks without order"
    /// is NOT an interpretable state — missing or non-List `order` is
    /// corruption and errors.
    fn order_list(&self) -> Result<automerge::ObjId> {
        self.typed_at(&ROOT, FIELD_ORDER, ObjType::List)?
            .context("malformed report doc: blocks map present but order list missing")
    }

    /// A block entry (`Map`) under the blocks map: `Ok(None)` when the
    /// id is absent, `Err` when present but not a Map.
    fn entry_at(&self, blocks_id: &automerge::ObjId, id: &str) -> Result<Option<automerge::ObjId>> {
        self.typed_at(blocks_id, id, ObjType::Map)
    }
}

/// Replace a block's Text object without Automerge's general Myers diff.
/// Report writes are serialized and revision-checked before reaching this
/// helper, so replacing the child object preserves the same visible text while
/// keeping work linear for large repetitive input.
fn replace_text_object(
    doc: &mut AutoCommit,
    entry: &automerge::ObjId,
    replacement: &str,
) -> Result<()> {
    doc.delete(entry, KEY_TEXT)
        .context("delete superseded block text object")?;
    let text_id = doc
        .put_object(entry, KEY_TEXT, ObjType::Text)
        .context("create replacement block text object")?;
    doc.update_text(&text_id, replacement)
        .context("write replacement block text")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> WaveReportPayload {
        WaveReportPayload::new(
            "spec agent did a thing",
            "# Goal\n\nReplace the foo with the bar.\n\n# Progress\n\nfoo->bar.\n",
        )
    }

    // ----- #955 §5.2: doc_heads ---------------------------------------

    #[test]
    fn doc_heads_is_stable_across_save_load_round_trips() {
        // Restart-survival: the token must be a pure function of the
        // committed change graph, not of in-process state. Round-trip
        // through bytes (= what a process restart does) twice.
        let mut doc = ReportDoc::from_payload(&sample_payload());
        let token = doc.doc_heads();
        assert!(token.starts_with("ah1:"), "scheme-tagged token: {token}");

        let mut reloaded = ReportDoc::from_bytes(&doc.to_bytes()).unwrap();
        assert_eq!(reloaded.doc_heads(), token);
        let mut reloaded_again = ReportDoc::from_bytes(&reloaded.to_bytes()).unwrap();
        assert_eq!(reloaded_again.doc_heads(), token);
    }

    #[test]
    fn doc_heads_changes_on_any_edit() {
        let mut doc = ReportDoc::from_payload(&sample_payload());
        let before = doc.doc_heads();

        // Body edit → new head.
        doc.update(
            "spec agent did a thing",
            "# Goal

changed.
",
        )
        .unwrap();
        let after_body = doc.doc_heads();
        assert_ne!(after_body, before, "body edit must move the heads");

        // Summary-only edit → new head again.
        doc.update(
            "new summary",
            "# Goal

changed.
",
        )
        .unwrap();
        let after_summary = doc.doc_heads();
        assert_ne!(after_summary, after_body);

        // Re-reading without writing does not move the token.
        assert_eq!(doc.doc_heads(), after_summary);
    }

    /// Serialize a pre-#960 doc: `summary` + `body` Texts at ROOT,
    /// no `blocks`/`order`. Mirrors the old `from_payload` verbatim.
    fn legacy_doc_bytes(summary: &str, body: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        let summary_id = doc.put_object(&ROOT, FIELD_SUMMARY, ObjType::Text).unwrap();
        doc.update_text(&summary_id, summary).unwrap();
        let body_id = doc
            .put_object(&ROOT, LEGACY_FIELD_BODY, ObjType::Text)
            .unwrap();
        doc.update_text(&body_id, body).unwrap();
        doc.save()
    }

    #[test]
    fn from_payload_then_project_returns_original_values() {
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        let (summary, body) = doc.project().unwrap();
        assert_eq!(summary, payload.summary);
        assert_eq!(body, payload.body);
        // Force a save round-trip too — project before save mustn't
        // depend on any pending-op state that disappears post-save.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s2, b2) = reloaded.project().unwrap();
        assert_eq!(s2, payload.summary);
        assert_eq!(b2, payload.body);
        // Two H1 sections → two prose blocks at rev 1, order matches.
        let index = reloaded.block_index().unwrap();
        assert_eq!(index.len(), 2);
        assert!(
            index
                .iter()
                .all(|(_, kind, rev)| kind == "prose" && *rev == 1)
        );
    }

    #[test]
    fn from_payload_reuses_hint_block_ids() {
        let mut payload = sample_payload();
        let hint = reassign_ids(&[], &split_body(&payload.body));
        payload.blocks = Some(hint.clone());
        let doc = ReportDoc::from_payload(&payload);
        let index = doc.block_index().unwrap();
        assert_eq!(
            index
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect::<Vec<_>>(),
            hint.iter()
                .map(|block| block.id.as_str())
                .collect::<Vec<_>>(),
            "PR1-derived ids survive the CRDT seed"
        );
    }

    #[test]
    fn from_payload_handles_empty_summary() {
        let payload = WaveReportPayload::new("", "# Goal\n");
        let mut doc = ReportDoc::from_payload(&payload);
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s, b) = reloaded.project().unwrap();
        assert_eq!(s, "");
        assert_eq!(b, "# Goal\n");
    }

    #[test]
    fn update_then_project_returns_new_values() {
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        doc.update("new summary", "# Heading\n\nnew body.\n")
            .unwrap();
        let (s, b) = doc.project().unwrap();
        assert_eq!(s, "new summary");
        assert_eq!(b, "# Heading\n\nnew body.\n");
        // And it survives a save round-trip.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s2, b2) = reloaded.project().unwrap();
        assert_eq!(s2, "new summary");
        assert_eq!(b2, "# Heading\n\nnew body.\n");
    }

    #[test]
    fn update_bumps_rev_only_for_changed_blocks() {
        let payload = WaveReportPayload::new("s", "# A\n\nalpha\n\n# B\n\nbeta\n");
        let mut doc = ReportDoc::from_payload(&payload);
        let before = doc.block_index().unwrap();
        assert_eq!(before.len(), 2);
        let (id_a, _, rev_a) = before[0].clone();
        let (id_b, _, rev_b) = before[1].clone();
        assert_eq!((rev_a, rev_b), (1, 1));

        // Edit only block A (mild edit — stays above the similarity
        // reuse threshold); B stays byte-identical.
        doc.update("s", "# A\n\nalpha edited\n\n# B\n\nbeta\n")
            .unwrap();
        assert_eq!(
            doc.block_rev(&id_a).unwrap(),
            Some(2),
            "changed block: rev+1"
        );
        assert_eq!(
            doc.block_rev(&id_b).unwrap(),
            Some(1),
            "untouched block: rev unchanged"
        );

        // Byte-identical rewrite: no rev movement at all.
        doc.update("s", "# A\n\nalpha edited\n\n# B\n\nbeta\n")
            .unwrap();
        assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
        assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));

        // Dropping a block deletes its entry; the survivor keeps id+rev.
        doc.update("s", "# B\n\nbeta\n").unwrap();
        assert_eq!(
            doc.block_rev(&id_a).unwrap(),
            None,
            "vanished block is deleted"
        );
        assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));
        assert_eq!(doc.project().unwrap().1, "# B\n\nbeta\n");
    }

    #[test]
    fn lazy_migration_preserves_projection_and_hint_ids() {
        let summary = "legacy summary";
        let body = "preamble\n\n# A\n\nalpha\n\n## B\n\nbeta\n";
        let bytes = legacy_doc_bytes(summary, body);

        // Read-only projection works before migration (legacy fallback).
        let unmigrated = ReportDoc::from_bytes(&bytes).unwrap();
        assert_eq!(
            unmigrated.project().unwrap(),
            (summary.to_string(), body.to_string())
        );
        assert!(unmigrated.blocks_snapshot().unwrap().is_empty());

        // Migrate with the PR1-derived JSON blocks as the id hint.
        let hint = reassign_ids(&[], &split_body(body));
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(
            doc.ensure_blocks_layout(Some(&hint)).unwrap(),
            "legacy doc migrates"
        );
        assert_eq!(
            doc.project().unwrap(),
            (summary.to_string(), body.to_string()),
            "projection is byte-identical"
        );
        assert_eq!(
            doc.block_index()
                .unwrap()
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect::<Vec<_>>(),
            hint.iter()
                .map(|block| block.id.as_str())
                .collect::<Vec<_>>(),
            "hint ids become the durable block ids"
        );
        // Legacy body is gone from the root.
        assert!(doc.0.get(&ROOT, LEGACY_FIELD_BODY).unwrap().is_none());

        // Idempotent: second call is a no-op, also across a save.
        assert!(!doc.ensure_blocks_layout(Some(&hint)).unwrap());
        let bytes2 = doc.to_bytes();
        let mut reloaded = ReportDoc::from_bytes(&bytes2).unwrap();
        assert!(!reloaded.ensure_blocks_layout(None).unwrap());
        assert_eq!(
            reloaded.project().unwrap(),
            (summary.to_string(), body.to_string())
        );
    }

    #[test]
    fn lazy_migration_without_hint_mints_ids() {
        let bytes = legacy_doc_bytes("s", "# A\n\nalpha\n");
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(doc.ensure_blocks_layout(None).unwrap());
        let index = doc.block_index().unwrap();
        assert_eq!(index.len(), 1);
        assert!(
            index[0].0.starts_with("b_"),
            "minted b_xxxx id, got {}",
            index[0].0
        );
        assert_eq!(index[0].1, "prose");
        assert_eq!(index[0].2, 1);
    }

    #[test]
    fn upsert_move_delete_block_round_trip() {
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
        let (id_a, _, _) = doc.block_index().unwrap()[0].clone();

        // Append a new block.
        let (id_b, rev_b) = doc.upsert_block(None, "prose", "# B\n\nbeta\n").unwrap();
        assert_eq!(rev_b, 1);
        assert!(id_b.starts_with("b_"));
        assert_ne!(id_b, id_a);
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha\n# B\n\nbeta\n");

        // Replace an existing block: rev bumps, content splices.
        let (same_id, rev) = doc
            .upsert_block(Some(&id_a), "prose", "# A\n\nalpha v2\n")
            .unwrap();
        assert_eq!(same_id, id_a);
        assert_eq!(rev, 2);
        assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

        // Replace with byte-identical content is an idempotent no-op:
        // rev unchanged, content unchanged (#960 PR2 review).
        let (_, rev) = doc
            .upsert_block(Some(&id_a), "prose", "# A\n\nalpha v2\n")
            .unwrap();
        assert_eq!(rev, 2, "identical content: rev holds");
        assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

        // Unknown id errors.
        assert!(doc.upsert_block(Some("b_nope"), "prose", "x").is_err());

        // Move B to the front; rev untouched.
        doc.move_block(&id_b, 0).unwrap();
        assert_eq!(doc.project().unwrap().1, "# B\n\nbeta\n# A\n\nalpha v2\n");
        assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));
        // And back to the tail.
        doc.move_block(&id_b, 1).unwrap();
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");
        // Out-of-range and unknown-id are errors.
        assert!(doc.move_block(&id_b, 2).is_err());
        assert!(doc.move_block("b_nope", 0).is_err());

        // Delete B.
        doc.delete_block(&id_b).unwrap();
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n");
        assert_eq!(doc.block_rev(&id_b).unwrap(), None);
        assert!(doc.delete_block(&id_b).is_err(), "double delete errors");

        // Everything survives a save round-trip.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).unwrap();
        assert_eq!(reloaded.project().unwrap().1, "# A\n\nalpha v2\n");
        assert_eq!(
            reloaded.block_index().unwrap(),
            vec![(id_a, "prose".to_string(), 2)]
        );
    }

    #[test]
    fn large_repetitive_block_replacement_has_linear_time_bound() {
        let markdown = format!(
            "# Fixture\n\n{}\n\ncapture pending\n",
            "long-fixture-segment-".repeat(450)
        );
        assert!(markdown.len() > 9_000, "fixture must retain its scale");
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", &markdown));
        let id = doc.block_index().unwrap()[0].0.clone();

        let started = std::time::Instant::now();
        doc.upsert_block(Some(&id), "prose", "[entity](neige://wave/source#b_target)")
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "9 KB replacement regressed beyond the linear-time budget: {elapsed:?}"
        );
        assert_eq!(
            doc.project().unwrap().1,
            "[entity](neige://wave/source#b_target)"
        );
    }

    #[test]
    fn identical_update_is_a_noop_at_byte_level() {
        // Re-asserting the same content produces zero text ops and no
        // rev movement; bound the saved-size growth as a smoke check
        // that we're not silently rewriting the block map every call.
        let payload = sample_payload();
        let mut doc = ReportDoc::from_payload(&payload);
        let first = doc.to_bytes();
        doc.update(&payload.summary, &payload.body).unwrap();
        let second = doc.to_bytes();
        let r1 = ReportDoc::from_bytes(&first).unwrap();
        let r2 = ReportDoc::from_bytes(&second).unwrap();
        assert_eq!(
            r1.project().unwrap(),
            (payload.summary.clone(), payload.body.clone())
        );
        assert_eq!(r2.project().unwrap(), (payload.summary, payload.body));
        assert_eq!(
            r1.block_index().unwrap(),
            r2.block_index().unwrap(),
            "no-op update moves no revs"
        );
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
        // logically a sequence of Unicode scalar values. Verify the
        // block-map projection is byte-for-byte identical to the input
        // across multi-byte UTF-8, multi-codepoint emoji, and CRLF.
        let summary = "中文测试 🎉 🇨🇳";
        let body = "line1\r\nline2 中文 🎉 🇨🇳\r\n";
        let payload = WaveReportPayload::new(summary, body);

        let mut doc = ReportDoc::from_payload(&payload);
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
        let (s, b) = reloaded.project().unwrap();
        assert_eq!(s.as_bytes(), summary.as_bytes());
        assert_eq!(b.as_bytes(), body.as_bytes());

        // And the update path must preserve them too.
        let mut doc2 = ReportDoc::from_bytes(&bytes).expect("re-load for update");
        let new_summary = "新摘要 🚀 🇯🇵";
        let new_body = "第一行\r\n第二行 🎊\r\n";
        doc2.update(new_summary, new_body).unwrap();
        let (s2, b2) = doc2.project().unwrap();
        assert_eq!(s2.as_bytes(), new_summary.as_bytes());
        assert_eq!(b2.as_bytes(), new_body.as_bytes());
        let bytes2 = doc2.to_bytes();
        let reloaded2 = ReportDoc::from_bytes(&bytes2).expect("post-update round-trip");
        let (s3, b3) = reloaded2.project().unwrap();
        assert_eq!(s3.as_bytes(), new_summary.as_bytes());
        assert_eq!(b3.as_bytes(), new_body.as_bytes());
    }

    #[test]
    fn concurrent_fork_merge_preserves_both_edits() {
        // Fork two replicas off the same root; each edits a different
        // block via the wholesale `update` path; merge them and both
        // edits must survive. Block-granular storage is what makes
        // this clean — the edits land in two independent Text objects.
        let payload = WaveReportPayload::new("shared", "# A\n\nalpha\n\n# B\n\nbeta\n");
        let mut origin = ReportDoc::from_payload(&payload);
        let bytes = origin.to_bytes();

        let mut replica_a = ReportDoc::from_bytes(&bytes).unwrap();
        let mut replica_b = ReportDoc::from_bytes(&bytes).unwrap();

        replica_a
            .update("shared", "# A\n\nALPHA\n\n# B\n\nbeta\n")
            .unwrap();
        replica_b
            .update("shared", "# A\n\nalpha\n\n# B\n\nBETA\n")
            .unwrap();

        replica_a.0.merge(&mut replica_b.0).expect("merge replicas");
        let (merged_summary, merged_body) = replica_a.project().unwrap();
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

    // -- malformed-doc hardening (#960 PR2 review) -------------------

    /// A fresh raw doc with a valid `summary` Text at ROOT.
    fn raw_doc_with_summary(summary: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        let summary_id = doc.put_object(&ROOT, FIELD_SUMMARY, ObjType::Text).unwrap();
        doc.update_text(&summary_id, summary).unwrap();
        doc
    }

    #[test]
    fn malformed_dangling_order_id_errors_instead_of_panicking() {
        // `order` references a block id with no `blocks` entry.
        let mut raw = raw_doc_with_summary("s");
        raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_dead").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.project().unwrap_err();
        assert!(err.to_string().contains("no blocks entry"), "err = {err:#}");
        assert!(doc.blocks_snapshot().is_err());
        assert!(doc.block_index().is_err());
        // Mutating entry points surface the same error, no panic.
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(doc.update("s", "# A\n").is_err());
    }

    #[test]
    fn malformed_non_text_block_field_errors_instead_of_panicking() {
        // Block entry whose `text` is a scalar Str, not a Text object.
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, 1_u64).unwrap();
        raw.put(&entry, KEY_TEXT, "scalar, not Text").unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.project().unwrap_err();
        assert!(
            format!("{err:#}").contains("not a Text object"),
            "err = {err:#}"
        );
        assert!(doc.blocks_snapshot().is_err());
    }

    #[test]
    fn malformed_missing_summary_errors_instead_of_panicking() {
        // v2 layout without any `summary` at ROOT.
        let mut raw = AutoCommit::new();
        raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.project().unwrap_err();
        assert!(
            format!("{err:#}").contains("missing `summary`"),
            "err = {err:#}"
        );
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(doc.update("s", "# A\n").is_err(), "update must not panic");
        assert!(
            doc.update_with_hints("s", &split_body("# A\n"), &[])
                .is_err(),
            "update_with_hints must not panic"
        );
    }

    /// A raw doc with summary + a well-formed block entry under
    /// `blocks`, but NO `order` list.
    fn raw_doc_blocks_without_order() -> Vec<u8> {
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, 1_u64).unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        raw.save()
    }

    #[test]
    fn blocks_without_order_is_corruption_not_an_empty_report() {
        // "blocks present, order missing" is NOT an interpretable
        // state — it must error, never read as a valid empty report
        // (which a subsequent write would then clobber).
        let bytes = raw_doc_blocks_without_order();
        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.project().unwrap_err();
        assert!(
            format!("{err:#}").contains("order list missing"),
            "err = {err:#}"
        );
        assert!(doc.blocks_snapshot().is_err());
        assert!(doc.has_blocks_layout().is_err());
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(
            doc.update("s", "").is_err(),
            "an empty-body write over the corrupt doc must be refused"
        );
    }

    #[test]
    fn scalar_order_is_corruption_not_an_empty_report() {
        // `order` present but as a scalar Str instead of a List.
        let mut raw = raw_doc_with_summary("s");
        raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        raw.put(&ROOT, FIELD_ORDER, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.project().unwrap_err();
        assert!(format!("{err:#}").contains("not a List"), "err = {err:#}");
        assert!(doc.blocks_snapshot().is_err());
        assert!(doc.has_blocks_layout().is_err());
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(doc.update("s", "# A\n").is_err());
    }

    #[test]
    fn scalar_rev_is_corruption_not_block_not_found() {
        // Block entry whose `rev` is a Str: rev corruption must error
        // (Internal at the boundary), never fold into `Ok(None)` /
        // "block not found" (which callers map to BadRequest).
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, "three").unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.blocks_snapshot().unwrap_err();
        assert!(format!("{err:#}").contains("no Uint rev"), "err = {err:#}");
        assert!(doc.has_blocks_layout().is_err());
        assert!(
            doc.block_rev("b_0001").is_err(),
            "rev corruption must be an error, not Ok(None)"
        );
        // An unknown id on the same doc is still a clean None.
        assert_eq!(doc.block_rev("b_nope").unwrap(), None);
    }

    #[test]
    fn out_of_range_rev_is_corruption_not_saturation() {
        // rev stored as a Uint beyond u32::MAX: corruption, not a
        // silently saturated value (#960 PR2 review round 3).
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, u64::from(u32::MAX) + 1).unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.blocks_snapshot().unwrap_err();
        assert!(format!("{err:#}").contains("exceeds u32"), "err = {err:#}");
        assert!(doc.block_rev("b_0001").is_err());
        assert!(doc.has_blocks_layout().is_err());
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(
            doc.upsert_block(Some("b_0001"), "prose", "x\n").is_err(),
            "replace over a corrupt rev must be refused"
        );
    }

    #[test]
    fn duplicate_order_id_is_corruption() {
        // `order` lists the same id twice: projecting it would emit
        // the block twice — corruption, not an interpretable state.
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, 1_u64).unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        raw.insert(&order, 1, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.blocks_snapshot().unwrap_err();
        assert!(
            format!("{err:#}").contains("duplicate id b_0001 in order"),
            "err = {err:#}"
        );
        assert!(doc.has_blocks_layout().is_err());
    }

    #[test]
    fn hidden_blocks_entry_outside_order_is_corruption() {
        // The blocks map carries an entry `order` never lists: hidden,
        // unreachable state — the 1:1 count check must reject it.
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        for id in ["b_0001", "b_hidden"] {
            let entry = raw.put_object(&blocks, id, ObjType::Map).unwrap();
            raw.put(&entry, KEY_KIND, "prose").unwrap();
            raw.put(&entry, KEY_REV, 1_u64).unwrap();
            let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
            raw.update_text(&text_id, "# A\n").unwrap();
        }
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.blocks_snapshot().unwrap_err();
        assert!(
            format!("{err:#}").contains("blocks map has 2 entries but order lists 1"),
            "err = {err:#}"
        );
        assert!(doc.has_blocks_layout().is_err());
    }

    // -- duplicate id hints (#960 PR2 review) ------------------------

    #[test]
    fn duplicate_hint_ids_migrate_with_unique_order() {
        // Legacy migration fed a payload `blocks` cache in which two
        // blocks share one id: only the first occurrence may claim it;
        // the projection stays byte-exact and `order` is unique.
        let body = "# A\n\nalpha\n\n# B\n\nbeta\n";
        let hint = vec![
            ReportBlock {
                id: "b_dupe".to_string(),
                kind: "prose".to_string(),
                rev: 2,
                payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
            },
            ReportBlock {
                id: "b_dupe".to_string(),
                kind: "prose".to_string(),
                rev: 5,
                payload: json!({ "markdown": "# B\n\nbeta\n" }),
            },
        ];
        let bytes = legacy_doc_bytes("s", body);
        let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
        assert!(doc.ensure_blocks_layout(Some(&hint)).unwrap());
        assert_eq!(
            doc.project().unwrap(),
            ("s".to_string(), body.to_string()),
            "projection is byte-identical"
        );
        let ids: Vec<String> = doc
            .block_index()
            .unwrap()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "b_dupe", "first occurrence keeps the id");
        assert_ne!(ids[1], "b_dupe", "duplicate gets a fresh id");
        assert_eq!(
            ids.iter().collect::<HashSet<_>>().len(),
            ids.len(),
            "order ids are unique"
        );
    }

    // -- non-prose blocks (#960 PR3) ---------------------------------

    #[test]
    fn upsert_non_prose_block_stores_canonical_fence_and_snapshot_parses_it() {
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
        let payload = json!({ "src": "/apps/x", "height": 480 });
        let fence_text = calm_types::report_blocks::render_fence("app", &payload);
        let (id, rev) = doc.upsert_block(None, "app", &fence_text).unwrap();
        assert_eq!(rev, 1);

        // Projection = prose + canonical fence, byte-exact.
        let (_, body) = doc.project().unwrap();
        assert_eq!(body, format!("# A\n\nalpha\n{fence_text}"));
        // Snapshot recovers the JSON payload from the stored fence.
        let blocks = doc.blocks_snapshot().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].id, id);
        assert_eq!(blocks[1].kind, "app");
        assert_eq!(blocks[1].payload, payload);
        // And the projection invariant holds through a save.
        let bytes = doc.to_bytes();
        let reloaded = ReportDoc::from_bytes(&bytes).unwrap();
        assert_eq!(reloaded.project().unwrap().1, body);
        assert_eq!(reloaded.blocks_snapshot().unwrap()[1].payload, payload);

        // Identical fence replace is idempotent; changed payload bumps.
        let (_, rev) = doc.upsert_block(Some(&id), "app", &fence_text).unwrap();
        assert_eq!(rev, 1, "identical fence: rev holds");
        let changed = calm_types::report_blocks::render_fence("app", &json!({ "src": "/apps/y" }));
        let (_, rev) = doc.upsert_block(Some(&id), "app", &changed).unwrap();
        assert_eq!(rev, 2, "changed payload: rev+1");

        // Non-fence content for a non-prose kind is an invariant error.
        assert!(doc.upsert_block(Some(&id), "app", "not a fence\n").is_err());
        // Kind/fence mismatch too.
        assert!(doc.upsert_block(Some(&id), "table", &changed).is_err());
    }

    #[test]
    fn non_prose_text_that_is_not_a_fence_is_corruption() {
        let mut raw = raw_doc_with_summary("s");
        let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "chart.candles").unwrap();
        raw.put(&entry, KEY_REV, 1_u64).unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "just markdown, no fence\n")
            .unwrap();
        let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let bytes = raw.save();

        let doc = ReportDoc::from_bytes(&bytes).unwrap();
        let err = doc.blocks_snapshot().unwrap_err();
        assert!(
            format!("{err:#}").contains("not a well-formed neige-block fence"),
            "err = {err:#}"
        );
        assert!(doc.has_blocks_layout().is_err());
        // project() still works (it only concatenates text) — the flat
        // body is not gated on payload parseability.
        assert!(doc.project().is_ok());
    }

    #[test]
    fn wholesale_update_carrying_the_fence_verbatim_preserves_the_block() {
        // The calm-types alignment path: a Replace-style update whose
        // body contains the canonical fence byte-for-byte keeps id,
        // kind, payload and rev (the server-level stomp guard allows
        // exactly this shape through).
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
        let payload = json!({ "src": "/apps/x" });
        let fence_text = calm_types::report_blocks::render_fence("app", &payload);
        let (id, _) = doc.upsert_block(None, "app", &fence_text).unwrap();

        doc.update("s", &format!("# A\n\nalpha edited\n{fence_text}"))
            .unwrap();
        let blocks = doc.blocks_snapshot().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].rev, 2, "edited prose: rev+1");
        assert_eq!(blocks[1].id, id);
        assert_eq!(blocks[1].kind, "app");
        assert_eq!(blocks[1].rev, 1, "untouched fence: rev holds");
        assert_eq!(blocks[1].payload, payload);
    }

    #[test]
    fn write_blocks_layout_dedupes_duplicate_ids_defensively() {
        // Feed the seeding path duplicate ids directly (bypassing
        // `reassign_ids`, which already guarantees uniqueness): the
        // first occupant keeps the id, the rest are re-minted, and the
        // projection still concatenates every block byte-exactly.
        let mut raw = raw_doc_with_summary("s");
        let blocks = vec![
            ReportBlock {
                id: "b_dupe".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
            },
            ReportBlock {
                id: "b_dupe".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({ "markdown": "# B\n\nbeta\n" }),
            },
        ];
        ReportDoc::write_blocks_layout(&mut raw, &blocks);
        let doc = ReportDoc(raw);
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha\n\n# B\n\nbeta\n");
        let ids: Vec<String> = doc
            .block_index()
            .unwrap()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids[0], "b_dupe");
        assert_ne!(ids[1], "b_dupe");
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    }

    #[test]
    fn from_blocks_exact_preserves_order_ids_revs_and_payloads() {
        let blocks = vec![
            ReportBlock {
                id: "b_0001".into(),
                kind: "prose".into(),
                rev: 7,
                payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
            },
            ReportBlock {
                id: "b_0002".into(),
                kind: "app".into(),
                rev: 11,
                payload: json!({ "src": "/apps/x" }),
            },
        ];

        let doc = ReportDoc::from_blocks_exact("summary", &blocks).unwrap();
        assert_eq!(doc.doc_rev().unwrap(), 0);
        assert_eq!(doc.blocks_snapshot().unwrap(), blocks);
        assert_eq!(doc.project().unwrap().0, "summary");
    }

    #[test]
    fn from_blocks_exact_accepts_consistent_empty_snapshot() {
        let doc = ReportDoc::from_blocks_exact("", &[]).unwrap();
        assert_eq!(doc.doc_rev().unwrap(), 0);
        assert_eq!(doc.blocks_snapshot().unwrap(), Vec::<ReportBlock>::new());
        assert_eq!(doc.block_index().unwrap(), Vec::new());
        assert_eq!(doc.project().unwrap(), (String::new(), String::new()));
    }

    #[test]
    fn from_blocks_exact_rejects_duplicate_ids_before_layout_write() {
        let blocks = vec![
            ReportBlock {
                id: "b_dupe".into(),
                kind: "prose".into(),
                rev: 3,
                payload: json!({ "markdown": "first\n" }),
            },
            ReportBlock {
                id: "b_dupe".into(),
                kind: "prose".into(),
                rev: 9,
                payload: json!({ "markdown": "second\n" }),
            },
        ];

        let error = match ReportDoc::from_blocks_exact("summary", &blocks) {
            Ok(_) => panic!("duplicate ids must fail before the layout writer can remint them"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("duplicate block id b_dupe"));
    }
}
