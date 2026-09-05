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
//! Lexical, and deliberately so — no tool owns "which source lines write this
//! column", and the property is textual rather than semantic. It sees SQL
//! written as string literals, which is every statement in this repository. It
//! would not see a statement assembled at runtime from fragments, so the second
//! assertion pins that no such assembly exists: `QueryBuilder` and `format!`
//! are checked never to produce a write to this column.
//!
//! It cannot decide the interesting question — whether a writer takes its queue
//! from the row — and does not pretend to. It makes a new writer VISIBLE.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every production source file under `crates/`, tests excluded.
fn production_sources() -> Vec<PathBuf> {
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
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates"),
        &mut out,
    );
    out.sort();
    out
}

fn repo_relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every production function whose body contains a SQL statement that assigns
/// `handle_state_json`.
fn writers_of_handle_state() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for path in production_sources() {
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
                .or_else(|| trimmed.strip_prefix("pub fn "))
                .or_else(|| trimmed.strip_prefix("fn "))
            {
                current_fn = rest.split('(').next().unwrap_or("").trim().to_string();
            }
            // An ASSIGNMENT to the column, or an insert into the table.
            //
            // Not the bare column name: every SELECT in the session row mappers
            // lists it, and matching those would bury the writers in reads. Not
            // `SET handle_state_json` either — that misses the column when it
            // is one assignment among several in a multi-column `SET`, which is
            // how `session_refresh_deferred_placeholder_tx` writes it. The
            // `let` guard keeps Rust bindings of the same name out.
            let writes = (trimmed.contains("handle_state_json =") && !trimmed.starts_with("let "))
                || trimmed.contains("INSERT INTO worker_sessions");
            if writes && !current_fn.is_empty() {
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
/// `carried` — writes a queue that came from somewhere else. **There must be
/// none**; if a new writer belongs here, the transfers in #1449 have to be
/// re-argued before it is added.
/// `not-a-queue` — writes the column without touching `pending_queue`.
const FROZEN_WRITERS: &[(&str, &str)] = &[
    // Insert/refresh primitives: they write whatever their caller assembled,
    // and every caller below is itself classified.
    (
        "crates/calm-truth/src/db/sqlite/session_mirror.rs::session_refresh_deferred_placeholder_tx",
        "row",
    ),
    (
        "crates/calm-truth/src/db/sqlite/session_mirror.rs::session_set_handle_state_mirror_tx",
        "row",
    ),
    (
        "crates/calm-truth/src/db/sqlite/session_projection.rs::session_set_handle_state_of_any_runtime_tx",
        "row",
    ),
    (
        "crates/calm-truth/src/db/sqlite/session_projection.rs::session_set_handle_state_of_retired_runtime_tx",
        "row",
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
    assert!(
        !FROZEN_WRITERS.iter().any(|(_, kind)| *kind == "carried"),
        "a writer is classified `carried`, which is the shape #1449 exists to remove"
    );
}

/// The scan is lexical, so this pins the one thing that would hide a writer
/// from it: SQL assembled at runtime.
#[test]
fn no_production_code_assembles_a_write_to_handle_state_json() {
    for path in production_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            let assembled = (line.contains("format!") || line.contains("QueryBuilder"))
                && line.contains("handle_state_json");
            assert!(
                !assembled,
                "{}:{} assembles SQL mentioning `handle_state_json`; the writer inventory is a \
                 lexical scan and cannot see it",
                repo_relative(&path),
                n + 1
            );
        }
    }
}

/// The counter-fixture. A gate only ever observed green proves nothing, so the
/// scan is run against a tree containing a writer that is not in the inventory
/// and must report it.
#[test]
fn the_inventory_sees_a_writer_that_is_not_in_it() {
    // Two shapes, because the first rule this test had only saw the first:
    // a dedicated `SET handle_state_json`, and the column as one assignment
    // among several — which is how the deferred-placeholder refresh writes it,
    // and which the earlier rule silently missed.
    let probe = r#"
pub async fn a_brand_new_writer(tx: &mut Tx) -> Result<()> {
    sqlx::query("UPDATE worker_sessions SET handle_state_json = ?1 WHERE id = ?2")
        .execute(tx)
        .await?;
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
"#;
    let mut current_fn = String::new();
    let mut found = Vec::new();
    for line in probe.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("pub async fn ")
            .or_else(|| trimmed.strip_prefix("async fn "))
        {
            current_fn = rest.split('(').next().unwrap_or("").trim().to_string();
        }
        // The same rule the inventory runs, applied to the probe.
        let writes = (trimmed.contains("handle_state_json =") && !trimmed.starts_with("let "))
            || trimmed.contains("INSERT INTO worker_sessions");
        if writes && !current_fn.is_empty() {
            found.push(current_fn.clone());
        }
    }
    assert_eq!(
        found,
        vec![
            "a_brand_new_writer".to_string(),
            "a_writer_hiding_in_a_multi_column_set".to_string()
        ],
        "the classifier must see a writer it has never been told about, in both shapes"
    );
}
