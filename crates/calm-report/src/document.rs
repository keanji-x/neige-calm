//! CRDT storage for the track-report card: `summary` Text + `blocks` map + `order` list at ROOT; the payload JSON is a projection cache.
//! Changed block text is replaced wholesale, which loses one side if two replicas ever edit the same block concurrently.

use anyhow::{Context, Result, bail, ensure};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use serde_json::json;
use std::collections::{HashMap, HashSet};

use calm_types::report_blocks::tasks::normalize_legacy_terminal_task_block;
use calm_types::report_blocks::{
    BlockSlice, KIND_PROSE, flat_text, mint_id, parse_fence, reassign_ids, reassign_ids_with_hints,
    reassign_ids_with_hints_reserving, render_fence, split_body,
};

use calm_types::track_report::{ReportBlock, TrackReportPayload};

const FIELD_SUMMARY: &str = "summary";
const FIELD_BLOCKS: &str = "blocks";
const FIELD_ORDER: &str = "order";
/// Document-wide revision; absent on legacy docs (reads as zero).
const FIELD_DOC_REV: &str = "doc_rev";
/// Legacy body text object; only the migrator and the read-only projection fallback touch it.
const LEGACY_FIELD_BODY: &str = "body";
const KEY_KIND: &str = "kind";
const KEY_REV: &str = "rev";
/// Block text: markdown for prose, the canonical `neige-block` fence for non-prose kinds.
const KEY_TEXT: &str = "text";

/// Opaque CRDT document holding the track-report's `summary` + block map.
pub struct ReportDoc(AutoCommit);

impl ReportDoc {
    /// Seed a brand-new doc from a payload snapshot; block ids from the payload's `blocks` cache survive the seed.
    pub fn from_payload(payload: &TrackReportPayload) -> Self {
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
        // Not routed through `write_blocks_layout`, which may remint a duplicate id; ids are written byte-for-byte.
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

    /// Document revision; missing (legacy) reads as zero.
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

    /// Increment `doc_rev`; a last-writer-wins register, so callers must serialize mutations through the persist transaction.
    pub fn increment_doc_rev(&mut self) -> Result<u64> {
        let next = self.doc_rev()?.checked_add(1).context("doc_rev overflow")?;
        self.0
            .put(&ROOT, FIELD_DOC_REV, next)
            .context("write doc_rev")?;
        Ok(next)
    }

    /// Pure load — no migration; mutators must run [`Self::ensure_blocks_layout`] first.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let doc = AutoCommit::load(bytes).context("automerge load")?;
        Ok(Self(doc))
    }

    pub fn to_bytes(&mut self) -> Vec<u8> {
        self.0.save()
    }

    /// Opaque, restart-stable token over the doc's sorted Automerge heads; equality is the only defined operation.
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

    /// Lazily migrate a legacy doc to the v2 block layout and rename legacy terminal-task `goal` to `command`
    /// without bumping block revs; returns whether anything changed.
    pub fn ensure_blocks_layout(&mut self, hint_blocks: Option<&[ReportBlock]>) -> Result<bool> {
        let layout_changed = if self.blocks_map().context("probe blocks map")?.is_some() {
            false
        } else {
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
            true
        };
        let command_changed = self.normalize_legacy_terminal_commands()?;
        Ok(layout_changed || command_changed)
    }

    fn normalize_legacy_terminal_commands(&mut self) -> Result<bool> {
        let blocks_id = self
            .blocks_map()?
            .context("doc invariant: blocks map must exist after layout migration")?;
        let mut changed = false;
        for block in self.blocks_snapshot()? {
            let normalized = normalize_legacy_terminal_task_block(&block);
            if normalized == block {
                continue;
            }
            let entry = self
                .entry_at(&blocks_id, &block.id)?
                .with_context(|| format!("block {} vanished during task migration", block.id))?;
            replace_text_object(
                &mut self.0,
                &entry,
                &render_fence(&normalized.kind, &normalized.payload),
            )
            .with_context(|| format!("migrate terminal task block {}", block.id))?;
            changed = true;
        }
        Ok(changed)
    }

    /// Wholesale replace: realign `new_body` against the current block map; byte-identical content is a no-op.
    pub fn update(&mut self, new_summary: &str, new_body: &str) -> Result<()> {
        let summary_id = self.summary_text_id()?;
        self.0
            .update_text(&summary_id, new_summary)
            .context("update summary text")?;

        let current = self.blocks_snapshot()?;
        let aligned = reassign_ids(&current, &split_body(new_body));
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// Summary-only write; the block map is untouched.
    pub fn set_summary(&mut self, new_summary: &str) -> Result<()> {
        let summary_id = self.summary_text_id()?;
        self.0
            .update_text(&summary_id, new_summary)
            .context("update summary text")
    }

    /// Wholesale replace with per-slice id hints; hinted slices bind to their old block exactly.
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

    /// Wholesale replace that drops every current non-prose block. `new_body` is aligned onto the
    /// current prose blocks only, with every current data block's id reserved, so no block of the
    /// result can carry a dropped data block's id or inherit its kind.
    pub fn replace_dropping_data_blocks(
        &mut self,
        new_summary: &str,
        new_body: &str,
    ) -> Result<()> {
        let summary_id = self.summary_text_id()?;
        self.0
            .update_text(&summary_id, new_summary)
            .context("update summary text")?;

        let current = self.blocks_snapshot()?;
        let (prose, data): (Vec<ReportBlock>, Vec<ReportBlock>) = current
            .iter()
            .cloned()
            .partition(|block| block.kind == KIND_PROSE);
        let reserved: HashSet<String> = data.into_iter().map(|block| block.id).collect();
        let aligned =
            reassign_ids_with_hints_reserving(&prose, &split_body(new_body), &[], &reserved);
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// [`Self::update_with_hints`] bounded to `range` of the current block list (#1877 section replace):
    /// only that part is matched against `slices`, every other block is kept as it is, and the
    /// summary is untouched. Returns the blocks that now stand in `range`'s place.
    pub fn replace_range(
        &mut self,
        range: std::ops::Range<usize>,
        slices: &[BlockSlice],
        hints: &[Option<String>],
    ) -> Result<Vec<ReportBlock>> {
        let current = self.blocks_snapshot()?;
        ensure!(
            range.start <= range.end && range.end <= current.len(),
            "replace_range: {range:?} out of range (len {})",
            current.len()
        );
        let (before, rest) = current.split_at(range.start);
        let (old, after) = rest.split_at(range.len());
        let reserved: HashSet<String> = before
            .iter()
            .chain(after)
            .map(|block| block.id.clone())
            .collect();
        let replaced = reassign_ids_with_hints_reserving(old, slices, hints, &reserved);
        let aligned: Vec<ReportBlock> = before
            .iter()
            .chain(&replaced)
            .chain(after)
            .cloned()
            .collect();
        self.apply_aligned_blocks(&current, &aligned)?;
        Ok(replaced)
    }

    /// `(summary, body)` projection; a not-yet-migrated legacy doc projects `ROOT.body` unchanged.
    pub fn project(&self) -> Result<(String, String)> {
        let summary = self.text_at(&ROOT, FIELD_SUMMARY)?;
        let body = if let Some(blocks_id) = self.blocks_map()? {
            let mut body = String::new();
            for id in self.order_ids()? {
                let entry = self.entry_at(&blocks_id, &id)?.with_context(|| {
                    format!("malformed report doc: order id {id} has no blocks entry")
                })?;
                calm_types::report_blocks::append_block_text(
                    &mut body,
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

    /// `Ok(true)` for a well-formed v2 layout, `Ok(false)` only for the legal legacy shape; anything in between errors.
    pub fn has_blocks_layout(&self) -> Result<bool> {
        match self.blocks_map()? {
            None => Ok(false),
            Some(_) => {
                self.blocks_snapshot()?;
                Ok(true)
            }
        }
    }

    /// Typed snapshot of the block map in `order` order; a non-prose text that is not a well-formed fence errors.
    pub fn blocks_snapshot(&self) -> Result<Vec<ReportBlock>> {
        let Some(blocks_id) = self.blocks_map()? else {
            return Ok(Vec::new());
        };
        // `order` must be duplicate-free and cover the blocks map exactly.
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

    pub fn block_index(&self) -> Result<Vec<(String, String, u32)>> {
        Ok(self
            .blocks_snapshot()?
            .into_iter()
            .map(|block| (block.id, block.kind, block.rev))
            .collect())
    }

    /// `Ok(None)` for an unknown id; a malformed entry is `Err`, never folded into "not found".
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

    /// `id = None` mints a fresh block at the tail (`rev = 1`); `Some` replaces and bumps `rev`,
    /// except byte-identical content is a no-op returning the current rev.
    pub fn upsert_block(
        &mut self,
        id: Option<&str>,
        kind: &str,
        content: &str,
    ) -> Result<(String, u32)> {
        // A non-prose block's stored text IS its canonical fence.
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

    /// Automerge lists have no move op: delete + insert on `order`; the block's rev is untouched.
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
        debug_assert_eq!(
            seen.len(),
            blocks.len(),
            "write_blocks_layout: order must be duplicate-free"
        );
    }

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

    /// A missing or non-List `order` is corruption, never "empty".
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

    /// `Ok(None)` when absent; a present value of the wrong type is corruption and errors.
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

    /// `None` for a legacy doc.
    fn blocks_map(&self) -> Result<Option<automerge::ObjId>> {
        self.typed_at(&ROOT, FIELD_BLOCKS, ObjType::Map)
    }

    /// "blocks without order" is not an interpretable state — a missing `order` errors.
    fn order_list(&self) -> Result<automerge::ObjId> {
        self.typed_at(&ROOT, FIELD_ORDER, ObjType::List)?
            .context("malformed report doc: blocks map present but order list missing")
    }

    fn entry_at(&self, blocks_id: &automerge::ObjId, id: &str) -> Result<Option<automerge::ObjId>> {
        self.typed_at(blocks_id, id, ObjType::Map)
    }
}

/// Replaces the Text child object instead of diffing, keeping writes linear for large repetitive input.
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
mod tests;
