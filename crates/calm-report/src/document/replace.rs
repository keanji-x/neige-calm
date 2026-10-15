//! Wholesale and section replace of a [`ReportDoc`]: realign new block slices onto the current
//! block map, reusing ids and keeping the blocks a write does not own.

use anyhow::{Result, ensure};
use std::collections::{HashMap, HashSet};

use calm_types::report_blocks::{
    BlockSlice, KIND_PROSE, reassign_ids, reassign_ids_with_hints,
    reassign_ids_with_hints_reserving, split_body,
};
use calm_types::track_report::ReportBlock;

use super::ReportDoc;

impl ReportDoc {
    /// Wholesale replace: realign `new_body` against the current block map; byte-identical content is a no-op.
    pub fn update(&mut self, new_summary: &str, new_body: &str) -> Result<()> {
        self.set_summary(new_summary)?;

        let current = self.blocks_snapshot()?;
        let aligned = reassign_ids(&current, &split_body(new_body));
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// Wholesale replace with per-slice id hints; hinted slices bind to their old block exactly.
    pub fn update_with_hints(
        &mut self,
        new_summary: &str,
        slices: &[BlockSlice],
        hints: &[Option<String>],
    ) -> Result<()> {
        self.set_summary(new_summary)?;

        let current = self.blocks_snapshot()?;
        let aligned = reassign_ids_with_hints(&current, slices, hints);
        self.apply_aligned_blocks(&current, &aligned)
    }

    /// [`Self::update_with_hints`] over the blocks `rewritten` selects only (#2617: a whole write
    /// does not own task blocks). Every other block is kept as it is — id, kind, rev and text — right
    /// after the nearest preceding rewritten block that survives, else after the result's first
    /// block, or in front when no rewritten block preceded it. Its id is never minted again.
    pub fn update_with_hints_keeping(
        &mut self,
        new_summary: &str,
        slices: &[BlockSlice],
        hints: &[Option<String>],
        rewritten: impl Fn(&ReportBlock) -> bool,
    ) -> Result<()> {
        self.set_summary(new_summary)?;

        let current = self.blocks_snapshot()?;
        let (old, kept): (Vec<ReportBlock>, Vec<ReportBlock>) =
            current.iter().cloned().partition(|block| rewritten(block));
        let reserved: HashSet<String> = kept.iter().map(|block| block.id.clone()).collect();
        let replaced = reassign_ids_with_hints_reserving(&old, slices, hints, &reserved);
        // Slot `s` of a kept block: it goes right before `replaced[s]` (or at the end).
        let position: HashMap<&str, usize> = replaced
            .iter()
            .enumerate()
            .map(|(index, block)| (block.id.as_str(), index))
            .collect();
        let mut slots = Vec::with_capacity(kept.len());
        let (mut anchor, mut after_rewritten) = (None, false);
        for block in &current {
            if rewritten(block) {
                after_rewritten = true;
                if let Some(&index) = position.get(block.id.as_str()) {
                    anchor = Some(index);
                }
            } else {
                let slot = match anchor {
                    Some(index) => index + 1,
                    None if after_rewritten => replaced.len().min(1),
                    None => 0,
                };
                slots.push((slot, block));
            }
        }
        let mut aligned = Vec::with_capacity(replaced.len() + kept.len());
        for slot in 0..=replaced.len() {
            aligned.extend(
                slots
                    .iter()
                    .filter(|(at, _)| *at == slot)
                    .map(|(_, block)| (*block).clone()),
            );
            aligned.extend(replaced.get(slot).cloned());
        }
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
        self.set_summary(new_summary)?;

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

    /// [`Self::update_with_hints`] bounded to the `members` of `span` (#1877 section replace; a
    /// [`crate::sections::Section`]'s indices): only those are matched against `slices`. Every other
    /// block is kept as it is, the non-members of `span` (#2598: task blocks) move after the replaced
    /// blocks, and the summary is untouched. Returns the blocks that now stand in the members' place.
    pub fn replace_section_members(
        &mut self,
        span: std::ops::Range<usize>,
        members: &[usize],
        slices: &[BlockSlice],
        hints: &[Option<String>],
    ) -> Result<Vec<ReportBlock>> {
        let current = self.blocks_snapshot()?;
        ensure!(
            span.start <= span.end && span.end <= current.len(),
            "replace_section_members: {span:?} out of range (len {})",
            current.len()
        );
        ensure!(
            members.iter().all(|index| span.contains(index)),
            "replace_section_members: members {members:?} outside {span:?}"
        );
        let (before, rest) = current.split_at(span.start);
        let after = &rest[span.len()..];
        let (old, kept): (Vec<usize>, Vec<usize>) =
            span.clone().partition(|index| members.contains(index));
        let [old, kept] = [old, kept].map(|indices| {
            indices
                .into_iter()
                .map(|index| current[index].clone())
                .collect::<Vec<ReportBlock>>()
        });
        let reserved: HashSet<String> = before
            .iter()
            .chain(&kept)
            .chain(after)
            .map(|block| block.id.clone())
            .collect();
        let replaced = reassign_ids_with_hints_reserving(&old, slices, hints, &reserved);
        let aligned: Vec<ReportBlock> = before
            .iter()
            .chain(&replaced)
            .chain(&kept)
            .chain(after)
            .cloned()
            .collect();
        self.apply_aligned_blocks(&current, &aligned)?;
        Ok(replaced)
    }
}
