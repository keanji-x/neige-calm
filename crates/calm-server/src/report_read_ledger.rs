//! What each worker session last read of a report (#1877): the anchors of every agent report write,
//! which takes no revisions (#1883). A `neige_report_read` that returned text records exactly the
//! snapshot it rendered. Every own write projects knowledge from the transaction's pre-batch and
//! final snapshots, then publishes it before receipt enrichment with a generation check.
//! Process memory like [`crate::plugin_results`]: a kernel restart forgets it and the next write is
//! refused until the session reads again. Eviction is lazy: the TTL on every access, and a session's
//! first read drops every other session's entries of the same card (at most one session of a card
//! is live, so those were superseded or exited).
//! Known gap, out of scope: a foreign delete whose id is later reused at the same rev can match a
//! stale anchor; the same ABA exists for the REST `if_rev`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use calm_types::track_report::ReportBlock;

use crate::plugin_results::Clock;
use crate::report_sections::sections;
use std::sync::atomic::{AtomicU64, Ordering};

pub const TTL_MS: i64 = 2 * 60 * 60 * 1000;

/// One session's reads of one report, merged: the `docRev` of its latest read, and every block and
/// whole section any of its reads rendered, at the rev it rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRead {
    pub doc_rev: u64,
    /// The session has seen every block at `doc_rev`: set by a read that rendered all of them,
    /// cleared by a partial read at another docRev, kept across own commits that advanced `doc_rev`.
    pub whole: bool,
    pub blocks: HashMap<String, u32>,
    /// Section title → the ordered `(id, rev)` list of its members (never a task block, #2598); only
    /// sections witnessed by a whole section read or the committed knowledge projection.
    pub sections: HashMap<String, Vec<(String, u32)>>,
}

struct Entry {
    caller_card_id: String,
    read: LastRead,
    recorded_at: i64,
    generation: u64,
}

/// Keyed by `(worker session id, report card id)`.
pub struct ReadLedger {
    inner: Mutex<HashMap<(String, String), Entry>>,
    now: Clock,
    generation: AtomicU64,
}

impl Default for ReadLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ReadLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadLedger")
            .field("entries", &self.lock().len())
            .finish()
    }
}

impl ReadLedger {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::model::now_ms))
    }

    /// Test seam: an injectable clock for the TTL.
    pub fn with_clock(now: Clock) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            now,
            generation: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record one text-returning read: `blocks` is the snapshot at `doc_rev` it rendered from,
    /// `rendered` the ids of the blocks its text carries. A section counts as read only when all its
    /// member blocks were rendered; a read of every block replaces the record.
    pub fn record(
        &self,
        session_id: &str,
        caller_card_id: &str,
        report_card_id: &str,
        doc_rev: u64,
        blocks: &[ReportBlock],
        rendered: &[String],
    ) {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.retain(|(session, _), entry| {
            now - entry.recorded_at <= TTL_MS
                && (session == session_id || entry.caller_card_id != caller_card_id)
        });
        let entry = inner
            .entry((session_id.to_string(), report_card_id.to_string()))
            .or_insert_with(|| Entry {
                caller_card_id: caller_card_id.to_string(),
                read: LastRead {
                    doc_rev,
                    whole: false,
                    blocks: HashMap::new(),
                    sections: HashMap::new(),
                },
                recorded_at: now,
                generation: 0,
            });
        entry.recorded_at = now;
        entry.generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let seen = |block: &ReportBlock| rendered.contains(&block.id);
        let all_seen = blocks.iter().all(seen);
        if all_seen {
            entry.read.blocks.clear();
            entry.read.sections.clear();
        }
        entry.read.whole = all_seen || (entry.read.whole && entry.read.doc_rev == doc_rev);
        entry.read.doc_rev = doc_rev;
        for block in blocks.iter().filter(|block| seen(block)) {
            entry.read.blocks.insert(block.id.clone(), block.rev);
        }
        entry.read.sections.extend(certified_sections(blocks, seen));
    }

    /// Capture knowledge and its publication generation; no lock survives an await.
    pub fn capture(&self, session_id: &str, report_card_id: &str) -> Option<(u64, LastRead)> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.retain(|_, entry| now - entry.recorded_at <= TTL_MS);
        inner
            .get(&(session_id.into(), report_card_id.into()))
            .map(|entry| (entry.generation, entry.read.clone()))
    }

    /// A failed transaction never calls this. A newer read or commit wins over a late publish.
    pub fn publish(&self, session_id: &str, report_card_id: &str, generation: u64, read: LastRead) {
        let now = (self.now)();
        let mut inner = self.lock();
        if let Some(entry) = inner.get_mut(&(session_id.into(), report_card_id.into()))
            && entry.generation == generation
            && now - entry.recorded_at <= TTL_MS
        {
            entry.read = read;
            entry.recorded_at = now;
            entry.generation = self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// What `session_id` last read of the report, if anything (and not expired).
    pub fn last_read(&self, session_id: &str, report_card_id: &str) -> Option<LastRead> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.retain(|_, entry| now - entry.recorded_at <= TTL_MS);
        inner
            .get(&(session_id.to_string(), report_card_id.to_string()))
            .map(|entry| entry.read.clone())
    }
}

/// The shared section rule: a unique title and evidence for every final member.
/// Callers supply both content and (for commits) structural-placement evidence.
fn certified_sections(
    blocks: &[ReportBlock],
    witnessed: impl Fn(&ReportBlock) -> bool,
) -> HashMap<String, Vec<(String, u32)>> {
    let all = sections(blocks);
    all.iter()
        .filter(|section| {
            all.iter()
                .filter(|other| other.title == section.title)
                .count()
                == 1
                && section
                    .member_blocks(blocks)
                    .iter()
                    .all(|block| witnessed(block))
        })
        .map(|section| {
            (
                section.title.to_string(),
                section
                    .member_blocks(blocks)
                    .iter()
                    .map(|block| (block.id.clone(), block.rev))
                    .collect(),
            )
        })
        .collect()
}

/// Project only witnessed unchanged content and content authored by this commit. Section evidence
/// must cover every D0 structural source contributing to a final section, even when all individual
/// blocks were known: knowing content alone does not certify the section's membership.
pub(crate) fn project_commit(
    old: &LastRead,
    before_rev: u64,
    before: &[ReportBlock],
    after_rev: u64,
    after: &[ReportBlock],
    written: &[String],
    section_output_ids: &[String],
) -> LastRead {
    let anchored = old.doc_rev == before_rev;
    let mut read = LastRead {
        doc_rev: if anchored { after_rev } else { old.doc_rev },
        whole: old.whole && anchored,
        blocks: HashMap::new(),
        sections: HashMap::new(),
    };
    for block in after {
        let valid = before.iter().any(|prior| {
            prior.id == block.id
                && prior.rev == block.rev
                && old.blocks.get(&prior.id) == Some(&prior.rev)
        });
        if valid || written.contains(&block.id) {
            read.blocks.insert(block.id.clone(), block.rev);
        }
    }
    let sources = sections(before);
    let valid_sources = certified_sections(before, |_| true);
    read.sections = certified_sections(after, |block| {
        let known = read.blocks.get(&block.id) == Some(&block.rev);
        let placement = section_output_ids.contains(&block.id)
            || if let Some(source) = sources.iter().find(|source| {
                source
                    .member_blocks(before)
                    .iter()
                    .any(|prior| prior.id == block.id)
            }) {
                valid_sources
                    .get(source.title)
                    .is_some_and(|members| old.sections.get(source.title) == Some(members))
            } else if before.iter().any(|prior| prior.id == block.id) {
                // Outside every H1: content knowledge alone does not witness placement.
                old.whole && anchored
            } else {
                written.contains(&block.id)
            };
        known && placement
    });
    read
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::{KIND_TASK, reassign_ids, render_fence, split_body};
    use std::sync::atomic::{AtomicI64, Ordering};

    fn ids(blocks: &[ReportBlock], indices: &[usize]) -> Vec<String> {
        indices.iter().map(|i| blocks[*i].id.clone()).collect()
    }

    #[test]
    fn records_rendered_blocks_and_only_whole_sections() {
        let ledger = ReadLedger::new();
        let blocks = reassign_ids(&[], &split_body("intro\n# A\na\n## A1\nx\n# B\nb\n"));
        ledger.record("s", "planner", "report", 3, &blocks, &ids(&blocks, &[1, 3]));
        let read = ledger.last_read("s", "report").expect("recorded");
        assert_eq!(read.doc_rev, 3);
        assert!(
            !read.whole,
            "a partial read is not a read of the whole report"
        );
        assert_eq!(read.blocks.len(), 2);
        assert_eq!(
            read.sections.keys().collect::<Vec<_>>(),
            vec!["B"],
            "A was rendered only in part"
        );
        assert_eq!(ledger.last_read("other", "report"), None);
    }

    #[test]
    fn section_anchors_exclude_task_blocks() {
        let ledger = ReadLedger::new();
        let task = render_fence(KIND_TASK, &serde_json::json!({ "key": "k" }));
        let blocks = reassign_ids(&[], &split_body(&format!("# A\na\n{task}## A1\nx\n")));
        assert_eq!(blocks[1].kind, KIND_TASK, "premise: the task sits inside A");
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[0, 2]));
        let read = ledger.last_read("s", "report").expect("recorded");
        assert_eq!(
            read.sections.get("A"),
            Some(&vec![
                (blocks[0].id.clone(), blocks[0].rev),
                (blocks[2].id.clone(), blocks[2].rev)
            ]),
            "a read of A's members is a read of A; the task is no part of its anchor"
        );
    }

    #[test]
    fn expires_and_a_new_session_of_the_card_drops_its_predecessor() {
        let clock = Arc::new(AtomicI64::new(0));
        let now = clock.clone();
        let ledger = ReadLedger::with_clock(Arc::new(move || now.load(Ordering::SeqCst)));
        let blocks = reassign_ids(&[], &split_body("# A\na\n"));
        let all = ids(&blocks, &[0]);
        ledger.record("old", "planner", "report", 1, &blocks, &all);
        ledger.record("assistant", "assistant-card", "report", 1, &blocks, &all);
        ledger.record("new", "planner", "report", 1, &blocks, &all);
        assert_eq!(ledger.last_read("old", "report"), None, "superseded");
        assert!(ledger.last_read("assistant", "report").is_some());
        clock.store(TTL_MS + 1, Ordering::SeqCst);
        assert_eq!(ledger.last_read("new", "report"), None, "expired");
    }

    #[test]
    fn whole_is_a_read_of_every_block_at_the_recorded_doc_rev() {
        let ledger = ReadLedger::new();
        let blocks = reassign_ids(&[], &split_body("# A\na\n# B\nb\n"));
        let whole = |ledger: &ReadLedger| ledger.last_read("s", "report").expect("recorded").whole;
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[0]));
        assert!(!whole(&ledger), "a partial first read");
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[0, 1]));
        assert!(whole(&ledger), "a read of every block");
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[1]));
        assert!(whole(&ledger), "a partial read at the same docRev keeps it");
        let (generation, read) = ledger.capture("s", "report").unwrap();
        ledger.publish(
            "s",
            "report",
            generation,
            project_commit(&read, 1, &blocks, 2, &blocks, &[], &[]),
        );
        assert!(
            whole(&ledger),
            "an own commit that advanced the docRev keeps it"
        );
        assert_eq!(ledger.last_read("s", "report").unwrap().doc_rev, 2);
        ledger.record("s", "planner", "report", 3, &blocks, &ids(&blocks, &[1]));
        assert!(
            !whole(&ledger),
            "a partial read at another docRev clears it"
        );
    }

    #[test]
    fn own_commit_refreshes_knowledge_ttl() {
        let clock = Arc::new(AtomicI64::new(0));
        let now = clock.clone();
        let ledger = ReadLedger::with_clock(Arc::new(move || now.load(Ordering::SeqCst)));
        let blocks = reassign_ids(&[], &split_body("# A\na\n"));
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[0]));
        clock.store(TTL_MS - 1, Ordering::SeqCst);
        let (generation, old) = ledger.capture("s", "report").unwrap();
        ledger.publish(
            "s",
            "report",
            generation,
            project_commit(&old, 1, &blocks, 2, &blocks, &[], &[]),
        );
        clock.store(TTL_MS + 1, Ordering::SeqCst);
        assert_eq!(ledger.last_read("s", "report").unwrap().doc_rev, 2);
        clock.store(2 * TTL_MS, Ordering::SeqCst);
        assert!(ledger.last_read("s", "report").is_none());
    }

    #[test]
    fn a_late_publication_cannot_overwrite_a_newer_read_or_recreated_entry() {
        let ledger = ReadLedger::new();
        let blocks = reassign_ids(&[], &split_body("# A\na\n"));
        ledger.record("s", "planner", "report", 1, &blocks, &ids(&blocks, &[0]));
        let (generation, old) = ledger.capture("s", "report").unwrap();
        ledger.record("s", "planner", "report", 2, &blocks, &ids(&blocks, &[0]));
        ledger.publish("s", "report", generation, old.clone());
        assert_eq!(ledger.last_read("s", "report").unwrap().doc_rev, 2);
        ledger.record(
            "other",
            "planner",
            "report",
            3,
            &blocks,
            &ids(&blocks, &[0]),
        );
        ledger.record("s", "planner", "report", 4, &blocks, &ids(&blocks, &[0]));
        ledger.publish("s", "report", generation, old);
        assert_eq!(ledger.last_read("s", "report").unwrap().doc_rev, 4);
    }
}
