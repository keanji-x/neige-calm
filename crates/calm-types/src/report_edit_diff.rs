//! #1667 D1 — the block-level diff a `ReportEdited` observation renders
//! for the planner.
//!
//! The observation used to say "re-read the track state" and nothing
//! else, so every save by somebody else cost the planner a
//! `calm.report.read` plus a reply. This turns the observation into
//! information: which blocks were added, removed or modified between
//! the body the planner last knew (`body_before`) and the body it is
//! being told about (`body`), bounded so a wholesale rewrite cannot
//! flood the turn input.
//!
//! Pure and IO-free (it lives in the vocabulary crate and runs inside
//! `Observation::to_turn_text`). The slicing is *the* report slicing —
//! [`crate::report_blocks::split_body`] — so a "block" here is exactly a
//! block of `calm.report.blocks.*`; nothing is restated. Block ids are
//! not in either body: the caller that has the report's block snapshot
//! at hand position-aligns it with the `after` slices through
//! [`align_block_refs`] and passes the result to
//! [`render_report_diff_with_refs`], which then names each added or
//! modified block by id and rev (#1667 round-2 F1). The alignment is
//! all-or-nothing and best-effort — a snapshot that no longer projects
//! to `after` yields no refs rather than wrong ones — and a planner
//! that wants to write still `calm.report.read`s for the current
//! `docRev` / `if_rev`.
//!
//! Pairing. Prose blocks pair by their H1/H2 heading line (same heading
//! several times: by order of appearance); a headingless leading slice
//! pairs with the other side's headingless leading slice. Non-prose
//! fences pair by `kind`, and `task` fences by `kind` + payload `key`
//! first, then whatever is left over on both sides pairs by order so a
//! renamed key shows as `key: "a" -> "b"` instead of one removal plus
//! one unrelated addition.
//!
//! Bounds. A modified or added/removed prose block contributes at most
//! [`MAX_BLOCK_LINES`] excerpt lines and [`MAX_BLOCK_BYTES`]; the whole
//! rendering is at most [`MAX_TOTAL_BYTES`]. Anything cut ends in
//! [`TRUNCATED_MARKER`]. Non-prose fences never contribute their
//! payload text: only the kind and the names of the top-level JSON
//! keys whose values changed (a `task` additionally spells out `ready`
//! and `key`, old -> new, because those two decide whether the kernel
//! schedules something).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::report_blocks::{KIND_TASK, append_block_text, flat_text, parse_fence, split_body};
use crate::track_report::ReportBlock;

/// Excerpt lines per block.
pub const MAX_BLOCK_LINES: usize = 24;
/// Excerpt bytes per block.
pub const MAX_BLOCK_BYTES: usize = 2048;
/// Bytes for the whole rendering, marker included.
pub const MAX_TOTAL_BYTES: usize = 8192;
/// Appended (on its own line) wherever a bound cut something.
pub const TRUNCATED_MARKER: &str = "… (truncated)";

/// A block's identity in the body a diff was rendered against: the
/// `id` / `rev` pair `calm.report.read` returns for it, as of the edit
/// the diff describes. Persisted inside `Observation::ReportEdited`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportBlockRef {
    pub id: String,
    pub rev: u32,
}

/// Position-align a report's block snapshot with the slices this
/// module cuts `after` into, so each `after` slice can be named by its
/// block id and rev.
///
/// `Some` only when the snapshot is exactly the body being diffed: its
/// projection (`append_block_text` over `flat_text`, the same fold
/// `ReportDoc::project` and `calm.report.read` use) is byte-equal to
/// `after` AND every block is exactly one slice, i.e. slice `i` ends
/// where block `i + 1`'s text begins. Anything else — a later write
/// having landed between the event and the read, a prose block that
/// holds two headings and so splits in two, an empty block — yields
/// `None`: no refs at all rather than a shifted or invented id.
pub fn align_block_refs(after: &str, blocks: &[ReportBlock]) -> Option<Vec<ReportBlockRef>> {
    let mut projected = String::new();
    let mut starts = Vec::with_capacity(blocks.len());
    for block in blocks {
        // The separator step of the projection on its own: with empty
        // text `append_block_text` only inserts the line break an
        // unterminated preceding block needs, so the length afterwards
        // is where this block's text begins, and the second call cannot
        // insert another (the body now ends in that break or is empty).
        append_block_text(&mut projected, "");
        starts.push(projected.len());
        append_block_text(&mut projected, &flat_text(block));
    }
    if projected != after {
        return None;
    }
    let slices = split_body(after);
    if slices.len() != blocks.len() {
        return None;
    }
    let mut end = 0;
    for (index, slice) in slices.iter().enumerate() {
        end += slice.raw.len();
        let next_start = starts.get(index + 1).copied().unwrap_or(after.len());
        if end != next_start {
            return None;
        }
    }
    Some(
        blocks
            .iter()
            .map(|block| ReportBlockRef {
                id: block.id.clone(),
                rev: block.rev,
            })
            .collect(),
    )
}

/// Render the block-level diff from `before` to `after`, naming no
/// block by id.
pub fn render_report_diff(before: &str, after: &str) -> String {
    render_report_diff_with_refs(before, after, None)
}

/// Render the block-level diff from `before` to `after`. With `refs`
/// (from [`align_block_refs`], position-aligned with the `after`
/// slices) every added or modified block's line carries its id and
/// rev — `## modified: b_ffb8 (rev 2) `## Next` (-1/+1 lines)`; a
/// removed block has no after-side identity and reads as without refs.
pub fn render_report_diff_with_refs(
    before: &str,
    after: &str,
    refs: Option<&[ReportBlockRef]>,
) -> String {
    if before == after {
        return "No block-level changes (the body is byte-identical).\n".to_string();
    }
    let before_blocks = classify(before);
    let after_blocks = classify(after);
    let (pairs, unchanged) = pair_blocks(&before_blocks, &after_blocks);
    // A refs sequence that does not cover the `after` slices is not the
    // one `align_block_refs` produced for this body; ignore it whole.
    let refs = refs.filter(|refs| refs.len() == after_blocks.len());

    let added = pairs.iter().filter(|p| p.before.is_none()).count();
    let removed = pairs.iter().filter(|p| p.after.is_none()).count();
    let modified = pairs.len() - added - removed;

    let mut lines: Vec<String> = vec![format!(
        "Blocks: {added} added, {removed} removed, {modified} modified ({unchanged} unchanged)."
    )];
    for pair in &pairs {
        lines.push(String::new());
        let after_ref = refs.zip(pair.after_index).map(|(refs, index)| &refs[index]);
        lines.extend(render_pair(pair, after_ref));
    }
    bound_total(lines)
}

/// `b_ffb8 (rev 2) ` — the identity prefix of an added / modified
/// block's line; empty when the diff has no refs.
fn ref_prefix(after_ref: Option<&ReportBlockRef>) -> String {
    after_ref
        .map(|r| format!("{} (rev {}) ", r.id, r.rev))
        .unwrap_or_default()
}

struct Block {
    raw: String,
    shape: Shape,
}

enum Shape {
    /// `heading` is the H1/H2 line the slice starts with, if any.
    Prose {
        heading: Option<String>,
    },
    Fence {
        kind: String,
        payload: Value,
    },
}

impl Block {
    fn title(&self) -> String {
        match &self.shape {
            Shape::Prose { heading: Some(h) } => format!("`{h}`"),
            Shape::Prose { heading: None } => "(untitled prose)".to_string(),
            Shape::Fence { kind, payload } => match task_key(kind, payload) {
                Some(key) => format!("`{kind}` block key = \"{key}\""),
                None => format!("`{kind}` block"),
            },
        }
    }

    fn is_fence(&self) -> bool {
        matches!(self.shape, Shape::Fence { .. })
    }
}

fn task_key(kind: &str, payload: &Value) -> Option<String> {
    (kind == KIND_TASK)
        .then(|| payload.get("key").and_then(Value::as_str))
        .flatten()
        .map(str::to_string)
}

fn classify(body: &str) -> Vec<Block> {
    split_body(body)
        .into_iter()
        .map(|slice| {
            let raw = slice.raw;
            let shape = match parse_fence(&raw) {
                Some(fence) => Shape::Fence {
                    kind: fence.kind,
                    payload: fence.payload,
                },
                None => Shape::Prose {
                    heading: first_line(&raw)
                        .filter(|line| line.starts_with("# ") || line.starts_with("## "))
                        .map(|line| line.trim_end().to_string()),
                },
            };
            Block { raw, shape }
        })
        .collect()
}

fn first_line(raw: &str) -> Option<&str> {
    raw.lines().next()
}

struct Pair<'a> {
    before: Option<&'a Block>,
    after: Option<&'a Block>,
    /// Index of `after` in the `after` body's slice sequence — what a
    /// refs sequence is aligned by. `None` for a removal.
    after_index: Option<usize>,
}

/// Pair blocks across the two bodies; returns the changed pairs (in
/// `after` order, then removals in `before` order) and the count of
/// pairs whose raw text is identical.
fn pair_blocks<'a>(before: &'a [Block], after: &'a [Block]) -> (Vec<Pair<'a>>, usize) {
    let mut before_taken = vec![false; before.len()];
    let mut after_taken = vec![false; after.len()];
    let mut matched: Vec<(usize, usize)> = Vec::new();

    // Exact-key pairing: heading / kind(+task key), by order of appearance.
    for (ai, a) in after.iter().enumerate() {
        let key = pair_key(a);
        if let Some(bi) = before
            .iter()
            .enumerate()
            .position(|(bi, b)| !before_taken[bi] && pair_key(b) == key)
        {
            before_taken[bi] = true;
            after_taken[ai] = true;
            matched.push((bi, ai));
        }
    }
    // Leftover task fences pair by order so a renamed key is one edit.
    let leftover_tasks = |blocks: &'a [Block], taken: &[bool]| -> Vec<usize> {
        blocks
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                !taken[*i] && matches!(&b.shape, Shape::Fence { kind, .. } if kind == KIND_TASK)
            })
            .map(|(i, _)| i)
            .collect()
    };
    let before_tasks = leftover_tasks(before, &before_taken);
    let after_tasks = leftover_tasks(after, &after_taken);
    for (bi, ai) in before_tasks.into_iter().zip(after_tasks) {
        before_taken[bi] = true;
        after_taken[ai] = true;
        matched.push((bi, ai));
    }

    let mut unchanged = 0;
    let mut pairs: Vec<Pair<'a>> = Vec::new();
    // `after` order first: modifications and additions as the reader
    // would meet them scrolling the new document.
    for (ai, a) in after.iter().enumerate() {
        match matched.iter().find(|(_, m_ai)| *m_ai == ai) {
            Some((bi, _)) => {
                let b = &before[*bi];
                if b.raw == a.raw {
                    unchanged += 1;
                } else {
                    pairs.push(Pair {
                        before: Some(b),
                        after: Some(a),
                        after_index: Some(ai),
                    });
                }
            }
            None => pairs.push(Pair {
                before: None,
                after: Some(a),
                after_index: Some(ai),
            }),
        }
    }
    for (bi, b) in before.iter().enumerate() {
        if !before_taken[bi] {
            pairs.push(Pair {
                before: Some(b),
                after: None,
                after_index: None,
            });
        }
    }
    (pairs, unchanged)
}

fn pair_key(block: &Block) -> String {
    match &block.shape {
        Shape::Prose { heading } => format!("prose:{}", heading.as_deref().unwrap_or("")),
        Shape::Fence { kind, payload } => match task_key(kind, payload) {
            Some(key) => format!("fence:{kind}:{key}"),
            None => format!("fence:{kind}"),
        },
    }
}

fn render_pair(pair: &Pair<'_>, after_ref: Option<&ReportBlockRef>) -> Vec<String> {
    match (pair.before, pair.after) {
        (Some(b), Some(a)) => render_modified(b, a, after_ref),
        (None, Some(a)) => {
            let mut out = vec![format!(
                "## added: {}{} (+{} lines)",
                ref_prefix(after_ref),
                a.title(),
                line_count(&a.raw)
            )];
            if !a.is_fence() {
                out.extend(bound_block(a.raw.lines().map(|l| format!("+{l}"))));
            }
            out
        }
        (Some(b), None) => {
            let mut out = vec![format!(
                "## removed: {} (-{} lines)",
                b.title(),
                line_count(&b.raw)
            )];
            if !b.is_fence() {
                out.extend(bound_block(b.raw.lines().map(|l| format!("-{l}"))));
            }
            out
        }
        (None, None) => Vec::new(),
    }
}

fn render_modified(b: &Block, a: &Block, after_ref: Option<&ReportBlockRef>) -> Vec<String> {
    let prefix = ref_prefix(after_ref);
    match (&b.shape, &a.shape) {
        (
            Shape::Fence {
                kind: b_kind,
                payload: b_payload,
            },
            Shape::Fence {
                kind: a_kind,
                payload: a_payload,
            },
        ) => {
            let title = if b_kind == a_kind {
                a.title()
            } else {
                format!("{} -> {}", b.title(), a.title())
            };
            let mut out = vec![format!("## modified: {prefix}{title}")];
            let changed = changed_top_level_keys(b_payload, a_payload);
            out.push(format!(
                "fields changed: {}",
                if changed.is_empty() {
                    "(none at top level)".to_string()
                } else {
                    changed.into_iter().collect::<Vec<_>>().join(", ")
                }
            ));
            if a_kind == KIND_TASK || b_kind == KIND_TASK {
                out.push(format!(
                    "ready: {} -> {}",
                    scalar_text(b_payload.get("ready")),
                    scalar_text(a_payload.get("ready"))
                ));
                out.push(format!(
                    "key: {} -> {}",
                    scalar_text(b_payload.get("key")),
                    scalar_text(a_payload.get("key"))
                ));
            }
            out
        }
        // Pairing keys are prefixed by shape (`prose:` / `fence:`) and the
        // leftover pass only pairs task fences with task fences, so the
        // remaining arm is prose against prose: a line diff of the raw
        // slice text.
        _ => {
            let (removed, added) = changed_lines(&b.raw, &a.raw);
            let title = if pair_key(b) == pair_key(a) {
                a.title()
            } else {
                format!("{} -> {}", b.title(), a.title())
            };
            let mut out = vec![format!(
                "## modified: {prefix}{title} (-{}/+{} lines)",
                removed.len(),
                added.len()
            )];
            out.extend(bound_block(
                removed
                    .iter()
                    .map(|l| format!("-{l}"))
                    .chain(added.iter().map(|l| format!("+{l}"))),
            ));
            out
        }
    }
}

/// Top-level keys whose value differs or that exist on one side only.
fn changed_top_level_keys(before: &Value, after: &Value) -> BTreeSet<String> {
    let empty = serde_json::Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    b.keys()
        .chain(a.keys())
        .filter(|key| b.get(*key) != a.get(*key))
        .cloned()
        .collect()
}

fn scalar_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "(absent)".to_string(),
        Some(Value::String(s)) => format!("\"{s}\""),
        Some(other) => other.to_string(),
    }
}

fn line_count(raw: &str) -> usize {
    raw.lines().count()
}

/// The lines that differ, after trimming the common prefix and suffix
/// line runs: `(removed from before, added in after)`.
fn changed_lines<'a>(before: &'a str, after: &'a str) -> (Vec<&'a str>, Vec<&'a str>) {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let max_suffix = b.len().min(a.len()) - prefix;
    let suffix = b[prefix..]
        .iter()
        .rev()
        .zip(a[prefix..].iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    (
        b[prefix..b.len() - suffix].to_vec(),
        a[prefix..a.len() - suffix].to_vec(),
    )
}

/// Per-block excerpt bound: at most [`MAX_BLOCK_LINES`] lines and
/// [`MAX_BLOCK_BYTES`] bytes (newlines counted); ends with the marker
/// when anything was cut.
fn bound_block(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut bytes = 0;
    let mut cut = false;
    for line in lines {
        if out.len() >= MAX_BLOCK_LINES {
            cut = true;
            break;
        }
        let line = if line.len() > MAX_BLOCK_BYTES {
            cut = true;
            truncate_at_char_boundary(&line, MAX_BLOCK_BYTES).to_string()
        } else {
            line
        };
        if bytes + line.len() + 1 > MAX_BLOCK_BYTES {
            cut = true;
            break;
        }
        bytes += line.len() + 1;
        out.push(line);
    }
    if cut {
        out.push(TRUNCATED_MARKER.to_string());
    }
    out
}

/// Whole-rendering bound: join with newlines, cut at a line boundary
/// so that the result including the marker is at most
/// [`MAX_TOTAL_BYTES`].
fn bound_total(lines: Vec<String>) -> String {
    let mut out = String::new();
    for line in &lines {
        let needed = out.len() + line.len() + 1;
        if needed + TRUNCATED_MARKER.len() + 1 > MAX_TOTAL_BYTES {
            // Would this line still fit without leaving room for the
            // marker, and is it the last one? Then no cut is needed.
            let is_last = std::ptr::eq(line, lines.last().expect("non-empty"));
            if is_last && needed <= MAX_TOTAL_BYTES {
                out.push_str(line);
                out.push('\n');
                return out;
            }
            out.push_str(TRUNCATED_MARKER);
            out.push('\n');
            return out;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn truncate_at_char_boundary(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
