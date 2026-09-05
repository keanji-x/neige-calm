//! #1449 — the set of statements that write `worker_sessions.handle_state_json`.
//!
//! # Why this is frozen
//!
//! A runtime's pending queue lives on its row, and the rule that makes the
//! transfers in #1449 sound is that a writer of that column takes the queue
//! FROM the row rather than from a copy it has been carrying. That rule was
//! adopted after converting `spawn_side_effect`, and it was wrong for a whole
//! review round: `app_server_interact` writes the same column from the snapshot
//! frozen in `operations.tx_output_json` at mint time, so an operation
//! re-driven after a crash put a transferred queue back and the sentence was in
//! two places again.
//!
//! The mistake was not the missed site. It was declaring a rule about "the
//! writers" without enumerating them. This freezes the enumeration so that the
//! next writer has to be classified by a person instead of inheriting the claim.
//!
//! # What this can and cannot see
//!
//! Lexical, and line at a time. No tool owns "which source lines write this
//! column", and the property is textual rather than semantic, so a scan is the
//! right instrument — but it is worth being exact about its reach.
//!
//! It sees a write spelled out in a string literal. It does NOT see a statement
//! assembled across lines, and this repository does assemble SQL that way
//! (`WS_CARD_KEYED_RUNTIME_SELECT`, `PROJECTABLE_RUNTIMES_FOR_CARDS_SQL`) —
//! those are reads today, and nothing here would notice if one became a write.
//!
//! It also cannot decide the interesting question, whether a writer takes its
//! queue from the row; that is what the classification beside each entry is
//! for, and it is written by a person. What this gate does is make a new
//! literal writer VISIBLE.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every production source file under `crates/`, tests excluded.
fn production_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates"),
        &mut out,
    );
    out.sort();
    out
}

fn collect_sources(root: &Path, out: &mut Vec<PathBuf>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "target" || name == "tests" || name == "node_modules" {
                    continue;
                }
                walk(&path, out);
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }
    walk(root, out);
}

fn repo_relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Does this line write `worker_sessions.handle_state_json`?
///
/// An ASSIGNMENT to the column, or an insert into the table. Not the bare
/// column name: every SELECT in the session row mappers lists it, and matching
/// those would bury the writers in reads. Not `SET handle_state_json` either —
/// that misses the column when it is one assignment among several in a
/// multi-column `SET`, which is how `session_refresh_deferred_placeholder_tx`
/// writes it, and the counter-fixture below caught exactly that. The `let`
/// guard keeps Rust bindings of the same name out.
///
/// The scan and the counter-fixture both call THIS. A counter-fixture with its
/// own copy of the predicate stays green while the real one rots, which is a
/// gate that only ever proves itself.
fn line_writes_handle_state(trimmed: &str) -> bool {
    (trimmed.contains("handle_state_json =") && !trimmed.starts_with("let "))
        || trimmed.contains("INSERT INTO worker_sessions")
}

/// Every production function whose body contains a SQL statement that assigns
/// `handle_state_json`.
fn writers_of_handle_state() -> BTreeSet<String> {
    writers_in_files(production_sources())
}

/// The same scan, pointed at an arbitrary tree. The counter-fixture uses this
/// so that it exercises the real walk — prefixes and all — rather than a copy.
fn writers_in_tree(root: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    collect_sources(root, &mut files);
    files.sort();
    writers_in_files(files)
}

fn writers_in_files(files: Vec<PathBuf>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut current_fn = String::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed
                .strip_prefix("pub async fn ")
                .or_else(|| trimmed.strip_prefix("async fn "))
                .or_else(|| trimmed.strip_prefix("pub(super) async fn "))
                .or_else(|| trimmed.strip_prefix("pub(crate) async fn "))
                .or_else(|| trimmed.strip_prefix("pub(super) fn "))
                .or_else(|| trimmed.strip_prefix("pub(crate) fn "))
                .or_else(|| trimmed.strip_prefix("pub const fn "))
                .or_else(|| trimmed.strip_prefix("const fn "))
                .or_else(|| trimmed.strip_prefix("pub fn "))
                .or_else(|| trimmed.strip_prefix("fn "))
            {
                current_fn = rest.split('(').next().unwrap_or("").trim().to_string();
            }
            if line_writes_handle_state(trimmed) && !current_fn.is_empty() {
                found.insert(format!("{}::{current_fn}", repo_relative(&path)));
            }
        }
    }
    found
}

/// The frozen inventory, each entry classified.
///
/// `row` — takes the queue it writes from the runtime's own row (directly, or
/// from a snapshot this transaction just read from it).
/// `carried` — writes a queue that did not come from the row. Each one needs a
/// written argument for why it cannot resurrect a transferred queue; there is
/// one today and its argument is next to it.
/// `not-a-queue` — writes the column without touching `pending_queue`.
const FROZEN_WRITERS: &[(&str, &str)] = &[
    // Insert/refresh primitives. They write whatever their caller assembled,
    // and their callers are NOT in this list: the scan only sees functions that
    // contain a SQL literal, so the Rust-level writers — `persist_snapshot_inner`,
    // `spawn_side_effect`, the deferred arm's clearing write — are invisible to
    // it. That is the gate's main blind spot and the reason the classification
    // beside each entry has to be read as being about this statement, not about
    // everything that reaches it.
    (
        "crates/calm-truth/src/db/sqlite/session_mirror.rs::session_refresh_deferred_placeholder_tx",
        "row",
    ),
    (
        "crates/calm-truth/src/db/sqlite/session_mirror.rs::session_set_handle_state_mirror_tx",
        "row",
    ),
    // `carried`, and it is the one writer that has to be: the give-back writes
    // message text read back out of `operations.tx_output_json`. It is sound
    // for the same reason the journal is — it writes ONLY ids the failing
    // runtime still holds, so it cannot resurrect a queue somebody else has
    // taken — but by this file's own definition the queue it writes did not
    // come from the row, and calling it `row` would be a false entry in the one
    // place that exists to keep this honest.
    (
        "crates/calm-truth/src/db/sqlite/session_projection.rs::session_set_handle_state_of_any_runtime_tx",
        "carried",
    ),
    // `carried`: its only caller, `persist_issuance_outcome`, serialises
    // `snapshot_for(inner)` — the run loop's in-process queue. That is sound
    // because it writes what this runtime still owes after its own drain, but
    // it is not the row, and the consequence is real: on the `turn/start` error
    // arm it writes a re-buffered batch back onto a row the harvest has already
    // taken from, which is why the give-back has to be idempotent against the
    // source row.
    (
        "crates/calm-truth/src/db/sqlite/session_projection.rs::session_set_handle_state_of_retired_runtime_tx",
        "carried",
    ),
    (
        "crates/calm-truth/src/db/sqlite/session_row.rs::session_insert_tx",
        "row",
    ),
    // The scheduler edits one unrelated key with `json_set` / `json_remove`.
    (
        "crates/calm-server/src/scheduler/mod.rs::mark_running_timeout_cleanup_tx",
        "not-a-queue",
    ),
    (
        "crates/calm-server/src/scheduler/mod.rs::clear_timeout_worker_cleanup_marker",
        "not-a-queue",
    ),
];

#[test]
fn every_writer_of_handle_state_json_is_classified() {
    let found = writers_of_handle_state();
    let frozen: BTreeSet<String> = FROZEN_WRITERS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    let unclassified: Vec<&String> = found.difference(&frozen).collect();
    assert!(
        unclassified.is_empty(),
        "a new writer of `worker_sessions.handle_state_json` appeared. Classify it: does it take \
         the queue it writes FROM the runtime's own row, or from a copy it was carrying? A \
         carried queue re-opens #1449. New writers: {unclassified:#?}"
    );
    let vanished: Vec<&String> = frozen.difference(&found).collect();
    assert!(
        vanished.is_empty(),
        "a frozen writer is gone; update the inventory deliberately: {vanished:#?}"
    );
    for (name, kind) in FROZEN_WRITERS {
        assert!(
            matches!(*kind, "row" | "carried" | "not-a-queue"),
            "{name} has an unknown classification {kind}"
        );
    }
}

/// The counter-fixture. A gate only ever observed green proves nothing, so the
/// real scanner is pointed at a tree containing a writer it has never been told
/// about and must report it.
///
/// It calls `writers_of_handle_state` itself rather than re-running the
/// predicate: an earlier version of this test copied the outer walk, so
/// deleting a prefix from the real scanner's list — `pub(super) async fn`, the
/// one `session_refresh_deferred_placeholder_tx` and
/// `session_set_handle_state_mirror_tx` need — left it green.
#[test]
fn the_inventory_sees_a_writer_that_is_not_in_it() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let crates = dir.path().join("crates").join("probe").join("src");
    std::fs::create_dir_all(&crates).expect("probe tree");
    std::fs::write(
        crates.join("lib.rs"),
        r#"
pub(super) async fn a_writer_hiding_behind_a_visibility_prefix(tx: &mut Tx) -> Result<()> {
    sqlx::query("UPDATE worker_sessions SET handle_state_json = ?1 WHERE id = ?2")
        .execute(tx)
        .await?;
    Ok(())
}

pub(crate) fn a_synchronous_writer_behind_another_prefix(tx: &mut Tx) -> Result<()> {
    sqlx::query("UPDATE worker_sessions SET handle_state_json = ?1 WHERE id = ?2").execute(tx)?;
    Ok(())
}

pub async fn a_writer_hiding_in_a_multi_column_set(tx: &mut Tx) -> Result<()> {
    sqlx::query(
        r"UPDATE worker_sessions
             SET state = ?1,
                 handle_state_json = ?2
           WHERE id = ?3",
    )
    .execute(tx)
    .await?;
    Ok(())
}
"#,
    )
    .expect("write probe");

    let found = writers_in_tree(crates.parent().unwrap().parent().unwrap());
    let names: BTreeSet<String> = found
        .iter()
        .map(|entry| entry.rsplit("::").next().unwrap_or(entry).to_owned())
        .collect();
    assert!(
        names.contains("a_writer_hiding_behind_a_visibility_prefix"),
        "the scanner must see a writer behind a visibility prefix — dropping one prefix from \
         its list is exactly how the two mirror writers would vanish: {names:#?}"
    );
    assert!(
        names.contains("a_writer_hiding_in_a_multi_column_set"),
        "and one that assigns the column inside a multi-column SET: {names:#?}"
    );
    assert!(
        names.contains("a_synchronous_writer_behind_another_prefix"),
        "and a non-async one behind a different visibility prefix — a prefix missing from the \
         scanner's list does not hide the writer, it files it under the PREVIOUS function's \
         name, which then matches the frozen inventory and keeps the gate green: {names:#?}"
    );
}
