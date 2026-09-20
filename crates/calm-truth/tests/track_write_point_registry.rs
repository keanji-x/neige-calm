//! Hygiene check on writers of `tracks.workspace_*`: surfaces every `.rs` string literal that writes a
//! workspace column so a new writer is visible in review. It is not a security boundary and cannot
//! prove that only one writer exists.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};

/// Columns owned by `tracks` alone, so no table-name reasoning is needed.
const WORKSPACE_COLUMNS: [&str; 3] = ["workspace_kind", "workspace_path", "workspace_frozen_at"];

/// SQL write keywords, matched on word boundaries so `updated_at` is not a write; `update`/`insert`
/// are bare so SQLite's conflict-clause variants (`UPDATE OR REPLACE`) are covered.
const WRITE_KEYWORDS: [&str; 4] = ["insert", "update", "delete", "replace"];

/// The production writers, pinned as exact normalized text; all of them live in one file. There is
/// deliberately no un-freeze writer: monotonicity is a property of the available statements.
const WRITER_FILE: &str = "crates/calm-truth/src/db/sqlite/track_workspace.rs";
const WRITER_STATEMENTS: &[(&str, &str)] = &[
    (
        "update tracks set workspace_path = ?1, workspace_kind = ?2, workspace_frozen_at = ?3 where id = ?4 and workspace_frozen_at is null",
        "The whole-value writer. The trailing predicate is the freeze latch \
         (#1147 S3): once a track has a stamp, neither kind nor path may change \
         again.",
    ),
    (
        "update tracks set workspace_frozen_at = ?1 where id = ?2 and workspace_frozen_at is null and (select c.kind from areas as c where c.id = tracks.area_id) <> 'system'",
        "`track_workspace_freeze_tx` — closes the latch, addressed by track. \
         Idempotent (never moves an existing stamp) and unable to clear one. \
         The system-area clause is the launchpad exception, expressed once here \
         rather than at each of the freeze points.",
    ),
];

/// Writes expected outside the production writer, by exact normalized text: `(file, statement, why)`.
const EXPECTED_OTHER_WRITES: &[(&str, &str, &str)] = &[
    (
        "crates/calm-server/tests/cases/today_launchpad.rs",
        "update tracks set purpose=null, workspace_path='/also-scrambled', workspace_kind='attached', workspace_frozen_at=null where id=?1",
        "Deliberately desynchronizes the row so the launchpad adopt branch has \
         to visibly rewrite it; without the scramble that assertion would pass \
         on leftovers from the create branch. #1147 S2 flipped the scrambled \
         values to `attached`/`99` because the launchpad's real workspace is \
         now `managed`/`NULL` — scrambling to the expected values would have \
         made the assertion vacuous. S3 flipped the STAMP back to NULL: the \
         latch makes a frozen launchpad un-repointable, and \
         `track_workspace_freeze_tx` excludes the system area precisely so that \
         row never gets one, so scrambling to 99 would fake an unreachable \
         state and turn the adopt branch into a 409.",
    ),
    (
        "crates/calm-server/tests/scheduler.rs",
        "insert into tracks(id,area_id,title,sort,workspace_kind,workspace_path,workspace_frozen_at,created_at,updated_at) select ?1,area_id,'replacement child',sort+0.25,workspace_kind,workspace_path,workspace_frozen_at,?2,?2 from tracks where id=?3",
        "Clones a track row wholesale, workspace included.",
    ),
    (
        "crates/calm-server/tests/scheduler.rs",
        "update tracks set workspace_kind='attached', workspace_path=?1, workspace_frozen_at=1 where id=?2",
        "#1147 S3 — forces `boot()`'s track to an attached, frozen, real \
         non-git directory. The production writer refuses a frozen row (the \
         latch) and has no un-freeze path, so a fixture that needs this state \
         has to write it out of band.",
    ),
    (
        "crates/calm-server/tests/scheduler.rs",
        "update tracks set workspace_kind='attached', workspace_path=?1, workspace_frozen_at=?2 where id=?3",
        "#1147 S3 — same reason as the entry above, with a real timestamp; \
         used by the child-track inheritance fixtures.",
    ),
    (
        "crates/calm-server/tests/scheduler.rs",
        "update tracks set workspace_kind='managed', workspace_path=?1, workspace_frozen_at=?2 where id=?3",
        "#1147 S3 — re-points a CHILD track, which S4 freezes at creation. The \
         fixture is deliberately simulating the thing the latch forbids, in \
         order to pin what happens to the child bootstrap's idempotency key \
         afterwards.",
    ),
    (
        "crates/calm-server/tests/no_double_spawn.rs",
        "update tracks set workspace_kind='attached', workspace_path=?1, workspace_frozen_at=1 where id=?2",
        "#1147 S3 — forces `boot()`'s track to an attached, frozen, real git \
         repository. Same reason as the `scheduler.rs` entries: the production \
         writer refuses a frozen row.",
    ),
    (
        "crates/calm-truth/src/db/sqlite/track_workspace_migration_tests.rs",
        "update tracks set workspace_frozen_at = null where id = ?1",
        "#1147 S3 — the ONLY un-freeze in the tree, and it is a test fixture. \
         The writer under test refuses a frozen row, so the test that pins \
         `kind`/`path`/`stamp` moving together has to open the latch first. \
         Production has no statement that can do this.",
    ),
    (
        "crates/calm-server/tests/cases/track_workspace_repoint.rs",
        "update tracks set workspace_kind='attached', workspace_path=?1, workspace_frozen_at=null where id=?2",
        "#1147 S3 — constructs `attached` + unfrozen, a state no route \
         produces, so the PATCH route's `kind` guard has a fixture of its own \
         rather than being shadowed by the freeze guard. Migration 0077's \
         comment names this exact state as the one a forgetful PATCH branch \
         would use to relocate a user's repository.",
    ),
    (
        "crates/calm-server/tests/cases/track_workspace_repoint.rs",
        "update tracks set parent_track_id=?1, workspace_frozen_at=?2 where id=?3",
        "#1147 S3 — reproduces the row shape S4's child-track adapter produces \
         (a child, frozen at creation) so the PATCH route's refusal is pinned \
         from this slice too. The adapter's own creation path is covered by \
         S4's tests; what is asserted here is the route's behaviour on that \
         state.",
    ),
];

/// This file, skipped by its own scan: its literals are the detector's test vectors.
const SELF: &str = "crates/calm-truth/tests/track_write_point_registry.rs";

fn normalize(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(start, _)| {
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let end = start + needle.len();
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

/// Workspace columns named by a normalized literal that also looks like a write; `None` for reads.
fn workspace_write_columns(normalized: &str) -> Option<Vec<&'static str>> {
    if !WRITE_KEYWORDS.iter().any(|k| contains_word(normalized, k)) {
        return None;
    }
    let columns: Vec<&'static str> = WORKSPACE_COLUMNS
        .into_iter()
        .filter(|column| contains_word(normalized, column))
        .collect();
    (!columns.is_empty()).then_some(columns)
}

/// Drop whole-line comments before parsing: tokenized, a `///` line becomes a `#[doc = "…"]` string literal.
fn strip_comment_lines(source: &str) -> String {
    source
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn literal_concat(stream: TokenStream) -> Option<String> {
    let tokens: Vec<_> = stream.into_iter().collect();
    let mut combined = String::new();
    let mut index = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            TokenTree::Literal(literal) => {
                combined.push_str(
                    &syn::parse_str::<syn::LitStr>(&literal.to_string())
                        .ok()?
                        .value(),
                );
                index += 1;
            }
            TokenTree::Punct(p) if p.as_char() == ',' => index += 1,
            _ => return None,
        }
    }
    Some(combined)
}

fn decoded_string_literals(stream: TokenStream, out: &mut Vec<String>) {
    let tokens: Vec<_> = stream.into_iter().collect();
    let mut index = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            TokenTree::Literal(literal) => {
                if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                    out.push(value.value());
                }
                index += 1;
            }
            TokenTree::Ident(ident)
                if ident == "concat"
                    && tokens.get(index + 1).is_some_and(
                        |t| matches!(t, TokenTree::Punct(p) if p.as_char() == '!'),
                    ) =>
            {
                if let Some(TokenTree::Group(group)) = tokens.get(index + 2) {
                    if let Some(combined) = literal_concat(group.stream()) {
                        out.push(combined);
                    }
                    decoded_string_literals(group.stream(), out);
                    index += 3;
                } else {
                    index += 1;
                }
            }
            TokenTree::Group(group) => {
                decoded_string_literals(group.stream(), out);
                index += 1;
            }
            _ => index += 1,
        }
    }
}

fn sql_literals(source: &str) -> Vec<String> {
    let stripped = strip_comment_lines(source);
    let Ok(stream) = stripped.parse::<TokenStream>() else {
        return vec![stripped];
    };
    let mut out = Vec::new();
    decoded_string_literals(stream, &mut out);
    out
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn workspace_members(root: &Path) -> Vec<PathBuf> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read workspace manifest");
    let table: toml::Value = manifest.parse().expect("parse workspace manifest");
    let members: Vec<PathBuf> = table
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("workspace.members array")
        .iter()
        .map(|m| root.join(m.as_str().expect("member is a string")))
        .collect();
    assert!(!members.is_empty(), "workspace member list was not decoded");
    members
}

fn rust_sources_below(path: &Path, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }
    for entry in fs::read_dir(path).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            rust_sources_below(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

fn scanned_sources() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut files = Vec::new();
    for member in workspace_members(&root) {
        rust_sources_below(&member.join("src"), &mut files);
        rust_sources_below(&member.join("tests"), &mut files);
    }
    files.sort();
    files.dedup();
    assert!(!files.is_empty(), "scanned zero files");
    files
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .expect("source below workspace root")
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn workspace_writes_are_the_ones_we_expect() {
    let root = workspace_root();
    let expected: BTreeSet<(&str, &str)> = EXPECTED_OTHER_WRITES
        .iter()
        .map(|(f, s, _)| (*f, *s))
        .chain(WRITER_STATEMENTS.iter().map(|(s, _)| (WRITER_FILE, *s)))
        .collect();

    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut problems = Vec::new();
    for file in scanned_sources() {
        let rel = relative(&root, &file);
        if rel == SELF {
            continue;
        }
        for literal in sql_literals(&fs::read_to_string(&file).expect("read source")) {
            let normalized = normalize(&literal);
            let Some(columns) = workspace_write_columns(&normalized) else {
                continue;
            };
            if expected.contains(&(rel.as_str(), normalized.as_str())) {
                seen.insert((rel.clone(), normalized));
            } else {
                problems.push(format!(
                    "{rel}: unexpected write of {columns:?}. If it is intentional, add it to \
                     EXPECTED_OTHER_WRITES verbatim with a reason — and read this file's module \
                     doc first, because being listed here is bookkeeping, not approval.\n  {normalized}"
                ));
            }
        }
    }
    for (file, statement) in &expected {
        if !seen.contains(&(file.to_string(), statement.to_string())) {
            problems.push(format!(
                "STALE entry — no such statement in {file}:\n  {statement}"
            ));
        }
    }
    assert!(problems.is_empty(), "\n\n{}", problems.join("\n\n"));
}

#[test]
fn detector_sees_the_shapes_that_defeated_earlier_versions() {
    let shapes = [
        (
            "const table name",
            "UPDATE {TRACKS_TABLE} SET workspace_path = ?1 WHERE id = ?2",
        ),
        (
            "lowercase",
            "update tracks set workspace_path = ?1 where id = ?2",
        ),
        (
            "reflowed",
            "UPDATE\n  tracks\n SET workspace_kind='managed'",
        ),
        (
            "schema-qualified",
            "UPDATE main.tracks SET workspace_path = ?1",
        ),
        (
            "conflict clause",
            "UPDATE OR REPLACE tracks SET workspace_frozen_at = NULL",
        ),
        (
            "insert-or-ignore",
            "INSERT OR IGNORE INTO tracks(workspace_path) VALUES(?1)",
        ),
        (
            "no table literal at all",
            "UPDATE {T} SET workspace_frozen_at = NULL",
        ),
    ];
    for (label, sql) in shapes {
        assert!(
            workspace_write_columns(&normalize(sql)).is_some(),
            "shape `{label}` not detected: {sql}"
        );
    }
}

#[test]
fn detector_ignores_reads_and_near_misses() {
    let benign = [
        (
            "select",
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id = ?1",
        ),
        (
            "updated_at is not a write",
            "SELECT workspace_path, updated_at FROM tracks",
        ),
        (
            "write touching no workspace column",
            "UPDATE tracks SET lifecycle='planning', updated_at=?1 WHERE id=?2",
        ),
        (
            "column list constant",
            "id, area_id, workspace_kind, workspace_path, workspace_frozen_at, updated_at",
        ),
    ];
    for (label, sql) in benign {
        assert_eq!(
            workspace_write_columns(&normalize(sql)),
            None,
            "benign shape `{label}` was flagged: {sql}"
        );
    }
}

#[test]
fn prose_is_not_evidence() {
    let source = "/// UPDATE tracks SET workspace_path = ?1 -- described, not executed\n\
                  //! also `update tracks set workspace_kind = ?1`\n\
                  fn f() { let _ = \"SELECT 1\"; }\n";
    assert!(
        sql_literals(source)
            .iter()
            .all(|l| workspace_write_columns(&normalize(l)).is_none())
    );
}

#[test]
fn the_check_itself_cannot_reach_a_database() {
    let source = fs::read_to_string(workspace_root().join(SELF)).expect("read self");
    // Identifiers, not substrings: a substring search would flag this file's own test vectors.
    let stream = strip_comment_lines(&source)
        .parse::<TokenStream>()
        .expect("gate source parses");
    let mut identifiers = BTreeSet::new();
    collect_identifiers(stream, &mut identifiers);
    for forbidden in [
        "sqlx",
        "SqlitePool",
        "SqlxRepo",
        "SqliteConnection",
        "Transaction",
    ] {
        assert!(
            !identifiers.contains(forbidden),
            "{SELF} now names `{forbidden}`; the self-exclusion assumes this file \
             cannot reach a database."
        );
    }
}

fn collect_identifiers(stream: TokenStream, out: &mut BTreeSet<String>) {
    for token in stream {
        match token {
            TokenTree::Ident(ident) => {
                out.insert(ident.to_string());
            }
            TokenTree::Group(group) => collect_identifiers(group.stream(), out),
            _ => {}
        }
    }
}
