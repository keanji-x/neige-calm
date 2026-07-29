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
//! // TODO(#960 PR3): non-prose blocks will store their payload as a
//! // deterministic JSON `Str` instead of markdown `Text`, and their
//! // flat representation becomes a fenced ```neige-block``` section.
//! // This slice only ever creates prose blocks.
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
    BlockSlice, mint_id, reassign_ids, reassign_ids_with_hints, split_body,
};

use crate::wave_report::{ReportBlock, WaveReportPayload};

/// Field key for the summary text object at the doc root.
const FIELD_SUMMARY: &str = "summary";
/// Field key for the block map at the doc root (v2 layout).
const FIELD_BLOCKS: &str = "blocks";
/// Field key for the block-id order list at the doc root (v2 layout).
const FIELD_ORDER: &str = "order";
/// Field key for the legacy (pre-#960) body text object. Only the
/// migrator and the read-only projection fallback may touch it.
const LEGACY_FIELD_BODY: &str = "body";
/// Block-entry key: block kind (`"prose"` today).
const KEY_KIND: &str = "kind";
/// Block-entry key: per-block optimistic-concurrency revision (Uint).
const KEY_REV: &str = "rev";
/// Block-entry key: the block's markdown content (Text).
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
        if self
            .0
            .get(&ROOT, FIELD_BLOCKS)
            .context("probe blocks map")?
            .is_some()
        {
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
    /// character-level `update_text` splice + `rev + 1`, new blocks
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
        let body = if let Some(blocks_id) = self.obj(&ROOT, FIELD_BLOCKS) {
            let mut body = String::new();
            for id in self.order_ids()? {
                let entry = self.obj(&blocks_id, &id).with_context(|| {
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

    /// True when the doc carries the v2 `blocks`/`order` layout
    /// (i.e. `ensure_blocks_layout` has run, or the doc was seeded
    /// post-#960). A `false` doc is a legacy pre-#960 shape.
    pub fn has_blocks_layout(&self) -> bool {
        self.obj(&ROOT, FIELD_BLOCKS).is_some()
    }

    /// Full typed snapshot of the block map in `order` order. The
    /// persist boundary mirrors this into `WaveReportPayload::blocks`.
    // TODO(#960 PR3): non-prose blocks will carry their stored JSON
    // payload here instead of `{ markdown }`.
    pub fn blocks_snapshot(&self) -> Result<Vec<ReportBlock>> {
        let Some(blocks_id) = self.obj(&ROOT, FIELD_BLOCKS) else {
            return Ok(Vec::new());
        };
        let mut blocks = Vec::new();
        for id in self.order_ids()? {
            let entry = self.obj(&blocks_id, &id).with_context(|| {
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
            let rev = u32::try_from(rev).unwrap_or(u32::MAX);
            let text = self
                .text_at(&entry, KEY_TEXT)
                .with_context(|| format!("malformed report doc: block {id} text field"))?;
            blocks.push(ReportBlock {
                id,
                kind,
                rev,
                payload: json!({ "markdown": text }),
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

    /// Current rev of a block, or `None` if the id doesn't exist.
    /// The `if_rev` optimistic-concurrency check reads this.
    pub fn block_rev(&self, id: &str) -> Option<u32> {
        let blocks_id = self.obj(&ROOT, FIELD_BLOCKS)?;
        let entry = self.obj(&blocks_id, id)?;
        self.0
            .get(&entry, KEY_REV)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.to_u64())
            .map(|rev| u32::try_from(rev).unwrap_or(u32::MAX))
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
        let blocks_id = self
            .obj(&ROOT, FIELD_BLOCKS)
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        match id {
            Some(id) => {
                let entry = self
                    .obj(&blocks_id, id)
                    .with_context(|| format!("block {id} not found"))?;
                let rev = self
                    .0
                    .get(&entry, KEY_REV)
                    .context("read block rev")?
                    .and_then(|(value, _)| value.to_u64())
                    .context("doc invariant: block entry has a Uint rev")?;
                let rev = u32::try_from(rev).unwrap_or(u32::MAX);
                let text_id = self
                    .obj(&entry, KEY_TEXT)
                    .context("doc invariant: block entry has a text field")?;
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
                self.0
                    .update_text(&text_id, content)
                    .context("update block text")?;
                Ok((id.to_string(), next_rev))
            }
            None => {
                let order_id = self
                    .obj(&ROOT, FIELD_ORDER)
                    .context("doc invariant: order list must exist")?;
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
        let order_id = self
            .obj(&ROOT, FIELD_ORDER)
            .context("doc invariant: order list must exist")?;
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
            .obj(&ROOT, FIELD_BLOCKS)
            .context("doc invariant: blocks map must exist (run ensure_blocks_layout)")?;
        let order_id = self
            .obj(&ROOT, FIELD_ORDER)
            .context("doc invariant: order list must exist")?;
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
    /// doc at block granularity.
    // TODO(#960 PR3): once non-prose blocks exist, the wholesale
    // `update` path must refuse to stomp them instead of relying on
    // `reassign_ids` preserving their payload; today only prose
    // blocks can exist so every block's content lives in its Text.
    fn apply_aligned_blocks(
        &mut self,
        current: &[ReportBlock],
        aligned: &[ReportBlock],
    ) -> Result<()> {
        let blocks_id = self
            .obj(&ROOT, FIELD_BLOCKS)
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
            let content = block_markdown(block);
            match old_by_id.get(block.id.as_str()) {
                Some(old) => {
                    let entry = self
                        .obj(&blocks_id, &block.id)
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
                    if block_markdown(old) != content {
                        let text_id = self
                            .obj(&entry, KEY_TEXT)
                            .context("doc invariant: block entry has a text field")?;
                        self.0
                            .update_text(&text_id, content)
                            .context("update block text")?;
                    }
                }
                None => {
                    Self::insert_block_entry(
                        &mut self.0,
                        &blocks_id,
                        &block.id,
                        &block.kind,
                        block.rev,
                        content,
                    );
                }
            }
        }
        let new_order: Vec<&str> = aligned.iter().map(|block| block.id.as_str()).collect();
        if self.order_ids()? != new_order {
            let order_id = self
                .obj(&ROOT, FIELD_ORDER)
                .context("doc invariant: order list must exist")?;
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
            let content = block_markdown(block);
            let id = if seen.insert(block.id.clone()) {
                block.id.clone()
            } else {
                // Duplicate id: first occupant wins, mint a fresh one.
                let minted = mint_id(content, index, &mut used);
                seen.insert(minted.clone());
                minted
            };
            Self::insert_block_entry(doc, &blocks_id, &id, &block.kind, block.rev, content);
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

    /// The block-id order, materialized as owned strings. Errors on a
    /// malformed doc whose `order` entries are not Str values.
    fn order_ids(&self) -> Result<Vec<String>> {
        let Some(order_id) = self.obj(&ROOT, FIELD_ORDER) else {
            return Ok(Vec::new());
        };
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

    /// Resolve a child object id off a parent. Returns `None` if the
    /// key is missing.
    fn obj(&self, parent: &automerge::ObjId, prop: &str) -> Option<automerge::ObjId> {
        self.0.get(parent, prop).ok().flatten().map(|(_, id)| id)
    }
}

/// A block's markdown content out of its `{ markdown }` payload.
// TODO(#960 PR3): non-prose payloads are not `{ markdown }`; their
// flat representation will be the deterministic fenced form.
fn block_markdown(block: &ReportBlock) -> &str {
    block
        .payload
        .get("markdown")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
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
        assert_eq!(doc.block_rev(&id_a), Some(2), "changed block: rev+1");
        assert_eq!(
            doc.block_rev(&id_b),
            Some(1),
            "untouched block: rev unchanged"
        );

        // Byte-identical rewrite: no rev movement at all.
        doc.update("s", "# A\n\nalpha edited\n\n# B\n\nbeta\n")
            .unwrap();
        assert_eq!(doc.block_rev(&id_a), Some(2));
        assert_eq!(doc.block_rev(&id_b), Some(1));

        // Dropping a block deletes its entry; the survivor keeps id+rev.
        doc.update("s", "# B\n\nbeta\n").unwrap();
        assert_eq!(doc.block_rev(&id_a), None, "vanished block is deleted");
        assert_eq!(doc.block_rev(&id_b), Some(1));
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
        assert_eq!(doc.block_rev(&id_a), Some(2));
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

        // Replace with byte-identical content is an idempotent no-op:
        // rev unchanged, content unchanged (#960 PR2 review).
        let (_, rev) = doc
            .upsert_block(Some(&id_a), "prose", "# A\n\nalpha v2\n")
            .unwrap();
        assert_eq!(rev, 2, "identical content: rev holds");
        assert_eq!(doc.block_rev(&id_a), Some(2));
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

        // Unknown id errors.
        assert!(doc.upsert_block(Some("b_nope"), "prose", "x").is_err());

        // Move B to the front; rev untouched.
        doc.move_block(&id_b, 0).unwrap();
        assert_eq!(doc.project().unwrap().1, "# B\n\nbeta\n# A\n\nalpha v2\n");
        assert_eq!(doc.block_rev(&id_b), Some(1));
        // And back to the tail.
        doc.move_block(&id_b, 1).unwrap();
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");
        // Out-of-range and unknown-id are errors.
        assert!(doc.move_block(&id_b, 2).is_err());
        assert!(doc.move_block("b_nope", 0).is_err());

        // Delete B.
        doc.delete_block(&id_b).unwrap();
        assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n");
        assert_eq!(doc.block_rev(&id_b), None);
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
}
