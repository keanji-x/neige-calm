//! Source-scan guard (#930): production deferred sqlite transactions must
//! be READ-ONLY — every writing transaction begins with
//! `begin_immediate_tx` (BEGIN IMMEDIATE).
//!
//! Why: the app's in-memory sqlite is a shared-cache database with
//! table-granularity locks and a single writer slot. A deferred
//! (`pool.begin()`) transaction that reads and then writes holds its read
//! locks while waiting for the writer slot, which closes a wait cycle
//! against any concurrent IMMEDIATE writer — sqlite's unlock_notify then
//! fails one side with plain `SQLITE_LOCKED` (6) "database is deadlocked"
//! (this took down `gh.pr.merge` in CI; see
//! `calm_truth::db::sqlite::deadlock_semantics_tests` for the pinned
//! upstream semantics and `operation::claim_completion_deadlock_tests`
//! for the production repro). A second IMMEDIATE instead parks at BEGIN
//! holding nothing, so writer-vs-writer can never cycle.
//!
//! If this test starts failing, the new deferred transaction must either
//! move to `begin_immediate_tx` (it writes) or be added to
//! `READ_ONLY_DEFERRED_ALLOWLIST` with a justification comment at the
//! call site (it provably only reads).
//!
//! Scanner hardening (#930 review): each file is lexically NORMALIZED
//! before scanning — line comments (incl. doc comments), nested block
//! comments, string literals (regular and raw, escape/hash aware) and
//! char literals are reduced to placeholders with quotes and line
//! structure preserved — then BOTH the `#[cfg(test)]` brace tracker and
//! the begin-pattern matcher run on that normalized text. Matching is
//! whitespace-collapsed (multiline call chains are caught) and flags:
//!   * `.begin(`  — method form on any receiver (incl. let-bound aliases)
//!   * `::begin(` — UFCS forms (`Connection::` / `Acquire::` / …)
//!   * `begin_with(` whose first argument is anything but the literal
//!     `"BEGIN IMMEDIATE"` (a `begin_with("BEGIN")` is a deferred tx in
//!     disguise)
//!
//! `begin_immediate_tx(` is excluded from all patterns by exact-token
//! matching. The allowlist is keyed by content (enclosing fn needle), not
//! by hit count, and the `tests.rs` / `*_tests.rs` name exemption is
//! verified fail-closed against each file's `mod` registration site.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Production files whose every deferred-begin hit is test-gated at the
/// MODULE REGISTRATION site (the file itself carries no `#[cfg(test)]`
/// marker). Each entry is verified fail-closed against the registering
/// file below.
const TEST_GATED_AT_REGISTRATION: &[(&str, &str, &str)] = &[(
    "calm-truth/src/db/sqlite/runtime_read_flip_support.rs",
    "calm-truth/src/db/sqlite/mod.rs",
    "#[cfg(test)]\nmod runtime_read_flip_support;",
)];

/// Documented READ-ONLY deferred transactions, keyed by CONTENT rather
/// than hit count (#930 review hardening): `(relative path, needle)`
/// where the needle — the enclosing `fn` name — must appear in the
/// normalized text within `ALLOWLIST_NEEDLE_WINDOW_LINES` lines above
/// EVERY matched begin in that file. A different deferred tx appearing
/// elsewhere in the same file therefore fails even when the total hit
/// count is unchanged.
///
/// A deferred transaction that performs no writes never competes for the
/// shared-cache writer slot, so it cannot be a hold-and-wait party.
const READ_ONLY_DEFERRED_ALLOWLIST: &[(&str, &str)] = &[
    // `wave_detail`: one consistent snapshot across wave/cards/overlays
    // SELECTs; commits without writing.
    ("calm-truth/src/db/sqlite/read.rs", "fn wave_detail("),
];

/// How far above a matched begin the allowlist needle may sit. The fn
/// signature is normally a handful of lines up (justification comment in
/// between); 25 leaves room for comment growth without letting a needle
/// vouch for begins in unrelated code further down the file.
const ALLOWLIST_NEEDLE_WINDOW_LINES: usize = 25;

#[test]
fn production_deferred_transactions_are_read_only_allowlisted() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir.parent().expect("crates dir").to_path_buf();

    let mut scanned = 0usize;
    let mut violations: Vec<String> = Vec::new();
    let mut consulted_allowlist: HashSet<&str> = HashSet::new();
    let mut name_exempted: Vec<(String, PathBuf)> = Vec::new();

    for crate_src in ["calm-server/src", "calm-truth/src"] {
        let root = crates_dir.join(crate_src);
        assert!(root.is_dir(), "scan root vanished: {root:?}");
        for path in rust_files(&root) {
            scanned += 1;
            let rel = path
                .strip_prefix(&crates_dir)
                .expect("path under crates dir")
                .to_string_lossy()
                .replace('\\', "/");
            // Test modules follow the `tests.rs` / `*_tests.rs` naming
            // convention crate-wide; they may open transactions however
            // the scenario demands. The exemption is by file name proper
            // (a bare `ends_with("tests.rs")` would also match e.g.
            // `contests.rs`) and every exempted file's `mod` registration
            // is verified `#[cfg(test)]`-gated below, fail closed.
            if is_test_named_file(&rel) {
                name_exempted.push((rel, path));
                continue;
            }
            if TEST_GATED_AT_REGISTRATION
                .iter()
                .any(|(file, _, _)| *file == rel)
            {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            let normalized = normalize_source(&src);
            let norm_lines: Vec<&str> = normalized.lines().collect();
            let hits = scan_deferred_begins(&flatten_production(&production_lines(&normalized)));
            if hits.is_empty() {
                continue;
            }
            match READ_ONLY_DEFERRED_ALLOWLIST
                .iter()
                .find(|(file, _)| *file == rel)
            {
                Some((file, needle)) => {
                    consulted_allowlist.insert(*file);
                    for (line, form) in &hits {
                        let hi = line - 1; // hits are 1-based; norm_lines 0-based
                        let lo = hi.saturating_sub(ALLOWLIST_NEEDLE_WINDOW_LINES);
                        if !norm_lines[lo..hi].iter().any(|l| l.contains(needle)) {
                            violations.push(format!(
                                "{rel}:{line}: {form} lacks allowlist needle {needle:?} within \
                                 {ALLOWLIST_NEEDLE_WINDOW_LINES} lines above — a different \
                                 deferred tx in an allowlisted file still fails"
                            ));
                        }
                    }
                }
                None => {
                    for (line, form) in &hits {
                        violations.push(format!("{rel}:{line}: {form}"));
                    }
                }
            }
        }
    }

    // Guard against the scan going vacuous through a path restructure
    // (the #917 failure mode for source-scanning tests).
    assert!(
        scanned > 100,
        "scan looks vacuous: only {scanned} .rs files visited"
    );
    assert!(
        !name_exempted.is_empty(),
        "scan looks vacuous: no `tests.rs` / `*_tests.rs` files found under the scan roots"
    );
    for (file, _) in READ_ONLY_DEFERRED_ALLOWLIST {
        assert!(
            consulted_allowlist.contains(file),
            "stale READ_ONLY_DEFERRED_ALLOWLIST entry (file moved or converted): {file}"
        );
    }
    assert!(
        violations.is_empty(),
        "production deferred transactions outside the read-only allowlist \
         (#930: writing transactions must use begin_immediate_tx):\n  {}",
        violations.join("\n  ")
    );

    // Fail-closed check for the name exemption: every `tests.rs` /
    // `*_tests.rs` file skipped above must have a `mod` registration that
    // is `#[cfg(test)]`-gated; a findable-but-ungated registration or an
    // unfindable one both fail loudly.
    let mut registration_violations: Vec<String> = Vec::new();
    for (rel, path) in &name_exempted {
        verify_test_registration_gated(&crates_dir, rel, path, &mut registration_violations);
    }
    assert!(
        registration_violations.is_empty(),
        "test-named files must be registered under #[cfg(test)] (#930 scan exemption \
         is fail-closed):\n  {}",
        registration_violations.join("\n  ")
    );

    // Fail-closed check for TEST_GATED_AT_REGISTRATION: the exempted
    // files really are `#[cfg(test)]` at their `mod` site.
    for (file, registrar, needle) in TEST_GATED_AT_REGISTRATION {
        let registrar_path = crates_dir.join(registrar);
        let registrar_src = std::fs::read_to_string(&registrar_path)
            .unwrap_or_else(|e| panic!("read {registrar}: {e}"));
        assert!(
            registrar_src.contains(needle),
            "{file} is exempted as test-gated, but {registrar} no longer \
             registers it under #[cfg(test)] (looked for {needle:?})"
        );
    }
}

/// File-name-proper test exemption: exactly `tests.rs`, or `*_tests.rs`.
fn is_test_named_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs" || name.ends_with("_tests.rs")
}

/// Locate the `mod` registration of a name-exempted test file in its
/// parent module (sibling `mod.rs`, the `<dir>.rs` file that owns the
/// directory, or a crate root `lib.rs`/`main.rs`) and require it to be
/// `#[cfg(test)]`-gated — i.e. absent from the registrar's production
/// lines. If no candidate contains the registration at all (e.g. a
/// `#[path]` rename), fail closed.
fn verify_test_registration_gated(
    crates_dir: &Path,
    rel: &str,
    path: &Path,
    violations: &mut Vec<String>,
) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| panic!("non-utf8 file stem: {rel}"));
    let dir = path.parent().expect("scanned file has a parent dir");
    let mut candidates: Vec<PathBuf> =
        vec![dir.join("mod.rs"), dir.join("lib.rs"), dir.join("main.rs")];
    if let (Some(grandparent), Some(dir_name)) =
        (dir.parent(), dir.file_name().and_then(|s| s.to_str()))
    {
        candidates.push(grandparent.join(format!("{dir_name}.rs")));
    }
    let registration = format!("mod {stem};");
    for candidate in candidates {
        if candidate == path || !candidate.is_file() {
            continue;
        }
        let src = std::fs::read_to_string(&candidate)
            .unwrap_or_else(|e| panic!("read {candidate:?}: {e}"));
        let normalized = normalize_source(&src);
        if !normalized.contains(&registration) {
            continue;
        }
        if production_lines(&normalized)
            .iter()
            .any(|(_, l)| l.contains(&registration))
        {
            let candidate_rel = candidate
                .strip_prefix(crates_dir)
                .unwrap_or(&candidate)
                .to_string_lossy()
                .replace('\\', "/");
            violations.push(format!(
                "{rel}: exempted by test-file naming, but its registration \
                 `{registration}` in {candidate_rel} is NOT #[cfg(test)]-gated"
            ));
        }
        return; // registration located (gated, or violation recorded)
    }
    violations.push(format!(
        "{rel}: exempted by test-file naming, but no `{registration}` registration \
         was found in its parent module candidates — fail closed"
    ));
}

// ---- lexical normalization -----------------------------------------------

/// Reduce Rust source to a scan-safe form: line comments (incl. doc
/// comments) and nested block comments become a single space; string
/// literals (regular and raw) keep their quotes but their contents become
/// a placeholder; char literals become `'c'`. Newlines are preserved
/// everywhere, so the output has the SAME line numbering as the input and
/// both the `#[cfg(test)]` brace tracker and the pattern matcher operate
/// on identical text. One deliberate carve-out: a string literal whose
/// content is exactly `BEGIN IMMEDIATE` is kept verbatim so the
/// `begin_with(` first-argument check can tell the one allowed statement
/// apart from deferred ones in disguise.
fn normalize_source(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let n = b.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while i < n {
        let c = b[i];
        if c == '/' && i + 1 < n && b[i + 1] == '/' {
            // Line comment (also `///`, `//!`): drop to EOL; the newline
            // itself is emitted by the next loop iteration.
            out.push(' ');
            i += 2;
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && b[i + 1] == '*' {
            // Block comment, nested per Rust rules (depth counter).
            out.push(' ');
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == '/' && i + 1 < n && b[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && i + 1 < n && b[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    if b[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        let prev_is_ident = i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_');
        if (c == 'r' || c == 'b')
            && !prev_is_ident
            && let Some(next) = consume_raw_string(&b, i, &mut out)
        {
            i = next;
            continue;
        }
        if c == '"' {
            i = consume_regular_string(&b, i, &mut out);
            continue;
        }
        // A `'` that is not a char literal is a lifetime or loop label —
        // plain code, falls through.
        if c == '\''
            && let Some(next) = consume_char_literal(&b, i)
        {
            out.push_str("'c'");
            i = next;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn push_string_literal_placeholder(out: &mut String, content: &str) {
    out.push('"');
    if content == "BEGIN IMMEDIATE" {
        out.push_str(content);
    } else {
        out.push('s');
        // Preserve embedded newlines so line numbering stays aligned.
        for ch in content.chars() {
            if ch == '\n' {
                out.push('\n');
            }
        }
    }
    out.push('"');
}

/// Try to consume `r"…"` / `r#"…"#` / `br…` starting at `start`; returns
/// the index just past the literal, or `None` if this isn't a raw string.
fn consume_raw_string(b: &[char], start: usize, out: &mut String) -> Option<usize> {
    let n = b.len();
    let mut j = start;
    if b[j] == 'b' {
        j += 1;
        if j >= n || b[j] != 'r' {
            return None;
        }
    }
    j += 1; // past 'r'
    let hash_start = j;
    while j < n && b[j] == '#' {
        j += 1;
    }
    let hashes = j - hash_start;
    if j >= n || b[j] != '"' {
        return None;
    }
    j += 1;
    let mut content = String::new();
    while j < n {
        // Terminator: `"` followed by `hashes` `#`s — for hash count = 0
        // (`r"…"`; raw strings have no escapes) the FIRST `"` closes.
        if b[j] == '"' && j + hashes < n && b[j + 1..j + 1 + hashes].iter().all(|c| *c == '#') {
            push_string_literal_placeholder(out, &content);
            return Some(j + 1 + hashes);
        }
        content.push(b[j]);
        j += 1;
    }
    // Unterminated (never for compiling sources): consume to EOF.
    push_string_literal_placeholder(out, &content);
    Some(n)
}

/// Consume a regular `"…"` (or the tail of a `b"…"`) with escape
/// handling; returns the index just past the closing quote.
fn consume_regular_string(b: &[char], start: usize, out: &mut String) -> usize {
    let n = b.len();
    let mut j = start + 1;
    let mut content = String::new();
    while j < n {
        if b[j] == '\\' && j + 1 < n {
            content.push(b[j]);
            content.push(b[j + 1]);
            j += 2;
            continue;
        }
        if b[j] == '"' {
            push_string_literal_placeholder(out, &content);
            return j + 1;
        }
        content.push(b[j]);
        j += 1;
    }
    push_string_literal_placeholder(out, &content);
    n
}

/// Distinguish a char literal (`'x'`, `'\n'`, `'\u{…}'`, `'{'`, `'"'`)
/// from a lifetime/label; returns the index just past the literal. Char
/// literals must be neutralized: `'{'` desyncs brace tracking and `'"'`
/// would otherwise open a phantom string.
fn consume_char_literal(b: &[char], start: usize) -> Option<usize> {
    let n = b.len();
    if start + 1 >= n {
        return None;
    }
    if b[start + 1] == '\\' {
        let mut j = start + 2;
        if j < n && b[j] == 'u' {
            if j + 1 >= n || b[j + 1] != '{' {
                return None;
            }
            j += 2;
            while j < n && b[j] != '}' {
                j += 1;
            }
            j += 1; // past '}'
        } else if j < n && b[j] == 'x' {
            j += 3; // x plus two hex digits
        } else {
            j += 1; // single escaped char: \n \' \\ …
        }
        if j < n && b[j] == '\'' {
            return Some(j + 1);
        }
        return None;
    }
    if start + 2 < n && b[start + 2] == '\'' && b[start + 1] != '\'' {
        return Some(start + 3);
    }
    None
}

// ---- #[cfg(test)] production filter ---------------------------------------

/// Return `(1-based line number, line)` for every line NOT inside a
/// test-gated item (see `is_test_gated_cfg`). Runs on NORMALIZED text, so
/// braces in strings/chars/comments can no longer desync the tracking.
/// Rustfmt (a CI gate) keeps attributes on their own line — the skip relies on it.
fn production_lines(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut lines = src.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        let trimmed = line.trim_start();
        if !is_test_gated_cfg(trimmed) {
            out.push((idx + 1, line));
            continue;
        }
        // Skip the gated item: stacked attributes, then one item — either
        // a `{ … }` block (fn/mod/impl) or a `;`-terminated declaration
        // (`use …;` / `mod …;`).
        let mut depth: i64 = 0;
        let mut saw_brace = false;
        for (_, item_line) in lines.by_ref() {
            let t = item_line.trim_start();
            if !saw_brace && depth == 0 && t.starts_with("#[") && !t.contains('{') {
                continue; // stacked attribute
            }
            depth += item_line.matches('{').count() as i64;
            depth -= item_line.matches('}').count() as i64;
            if item_line.contains('{') {
                saw_brace = true;
            }
            if saw_brace && depth <= 0 {
                break;
            }
            if !saw_brace && t.ends_with(';') {
                break;
            }
        }
    }
    out
}

/// True iff the line is `#[cfg(…)]` with `test` as a STANDALONE token —
/// not `"test-utils"`/`"testing"` (`-` is a word char) — and NO standalone
/// token `not` (however spaced: `not(…)`, `not (…)`): negation defeats
/// token-level reasoning (`not(any(test))` is production-only), so negated
/// predicates count as production. Fail closed: at worst a gated file
/// flags loudly and gets allowlisted.
fn is_test_gated_cfg(trimmed: &str) -> bool {
    let Some(pred) = trimmed.strip_prefix("#[cfg(") else {
        return false;
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let has_token = |tok| pred.split(|c: char| !is_word(c)).any(|t| t == tok);
    has_token("test") && !has_token("not")
}

// ---- whitespace-collapsed pattern matching ---------------------------------

/// Flatten production lines into a whitespace-collapsed char stream with
/// per-char line attribution, so multiline call chains
/// (`pool\n.begin\n()`) match like single-line ones. Non-consecutive
/// input lines (a skipped `#[cfg(test)]` region) are separated by a `\0`
/// barrier so no pattern can straddle the gap.
fn flatten_production(lines: &[(usize, &str)]) -> Vec<(char, usize)> {
    let mut flat: Vec<(char, usize)> = Vec::new();
    let mut prev_line: Option<usize> = None;
    let mut pending_space = false;
    for (line_no, line) in lines {
        if let Some(prev) = prev_line
            && *line_no != prev + 1
        {
            flat.push(('\0', *line_no));
            pending_space = false;
        }
        for ch in line.chars() {
            if ch.is_whitespace() {
                pending_space = true;
            } else {
                if pending_space && !flat.is_empty() {
                    flat.push((' ', *line_no));
                }
                pending_space = false;
                flat.push((ch, *line_no));
            }
        }
        pending_space = true; // the newline itself
        prev_line = Some(*line_no);
    }
    flat
}

/// Scan the flattened production stream for deferred-transaction begins.
/// Word-exact token matching: `begin` and `begin_with` are maximal ident
/// runs, so `begin_immediate_tx(` (and any `*begin*` identifier) never
/// matches. Returns `(1-based line of the begin token, form description)`.
fn scan_deferred_begins(flat: &[(char, usize)]) -> Vec<(usize, &'static str)> {
    fn is_ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let n = flat.len();
    let mut hits = Vec::new();
    let mut i = 0usize;
    while i < n {
        if !is_ident(flat[i].0) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        while j < n && is_ident(flat[j].0) {
            j += 1;
        }
        let word: String = flat[start..j].iter().map(|(c, _)| *c).collect();
        let line = flat[start].1;
        let mut k = j;
        while k < n && flat[k].0 == ' ' {
            k += 1;
        }
        if k < n && flat[k].0 == '(' {
            match word.as_str() {
                "begin" => {
                    let mut p = start;
                    let mut prev = None;
                    while p > 0 {
                        p -= 1;
                        if flat[p].0 != ' ' {
                            prev = Some(p);
                            break;
                        }
                    }
                    if let Some(p) = prev {
                        if flat[p].0 == '.' {
                            hits.push((line, "deferred `.begin(`"));
                        } else if flat[p].0 == ':' && p > 0 && flat[p - 1].0 == ':' {
                            hits.push((line, "deferred UFCS `::begin(`"));
                        }
                    }
                }
                // Only `begin_with("BEGIN IMMEDIATE")` — the literal as
                // the first argument — is an IMMEDIATE tx; any other
                // shape (other literal, variable, const, or the UFCS
                // `Connection::begin_with(conn, …)` where the statement
                // is the SECOND argument) flags, fail closed.
                "begin_with" if !begin_with_first_arg_is_immediate(flat, k) => {
                    hits.push((line, "`begin_with(` without a \"BEGIN IMMEDIATE\" literal"));
                }
                _ => {}
            }
        }
        i = j;
    }
    hits
}

/// True iff the argument list opening at `open_paren` starts with the
/// literal `"BEGIN IMMEDIATE"` followed by `)` or `,`. The normalizer
/// preserves exactly this string's content, so the check works on
/// normalized text.
fn begin_with_first_arg_is_immediate(flat: &[(char, usize)], open_paren: usize) -> bool {
    const LITERAL: &str = "\"BEGIN IMMEDIATE\"";
    let mut k = open_paren + 1;
    while k < flat.len() && flat[k].0 == ' ' {
        k += 1;
    }
    for expected in LITERAL.chars() {
        if k >= flat.len() || flat[k].0 != expected {
            return false;
        }
        k += 1;
    }
    while k < flat.len() && flat[k].0 == ' ' {
        k += 1;
    }
    matches!(flat.get(k).map(|(c, _)| *c), Some(')') | Some(','))
}

// ---- file walk --------------------------------------------------------------

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit_rust_files(root, &mut out);
    out
}

fn visit_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.is_dir() {
            visit_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

// ---- scanner self-tests ------------------------------------------------------
//
// Permanent negative/positive coverage for the hardened scanner itself;
// the transient injected-canary protocol from the #930 review lives in
// the PR notes, these pin the same behaviors in-tree.

fn scan_snippet(src: &str) -> Vec<(usize, &'static str)> {
    let normalized = normalize_source(src);
    scan_deferred_begins(&flatten_production(&production_lines(&normalized)))
}

#[test]
fn scanner_flags_method_ufcs_multiline_and_disguised_begin_with() {
    let hits = scan_snippet(concat!(
        "async fn f(pool: &P) {\n",
        "    let a = pool.begin().await;\n",
        "    let b = pool\n",
        "        .begin\n",
        "        ()\n",
        "        .await;\n",
        "    let c = sqlx::Acquire::begin(&mut conn).await;\n",
        "    let d = pool.begin_with(\"BEGIN\").await;\n",
        "    let e = pool.begin_with(\"BEGIN DEFERRED\").await;\n",
        "    let f = Connection::begin_with(&mut *conn, \"BEGIN IMMEDIATE\").await;\n",
        "}\n",
    ));
    let lines: Vec<usize> = hits.iter().map(|(l, _)| *l).collect();
    // Line 10 (UFCS begin_with) flags fail-closed: the IMMEDIATE literal
    // is not the first argument there.
    assert_eq!(lines, vec![2, 4, 7, 8, 9, 10], "{hits:?}");
}

#[test]
fn scanner_ignores_immediate_forms_comments_strings_and_test_gated() {
    let hits = scan_snippet(concat!(
        "async fn f(pool: &P) {\n",
        "    let t = begin_immediate_tx(pool).await;\n",
        "    let u = pool.begin_with(\"BEGIN IMMEDIATE\").await;\n",
        "    let v = pool.begin_with (\n",
        "        \"BEGIN IMMEDIATE\",\n",
        "    ).await;\n",
        "    // let tx = pool.begin().await;\n",
        "    /* pool.begin() */\n",
        "    let s = \"pool.begin() and begin_with(\\\"BEGIN\\\")\";\n",
        "    let r = r#\"pool.begin()\"#;\n",
        "}\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    fn g(pool: &P) {\n",
        "        let tx = pool.begin();\n",
        "    }\n",
        "}\n",
    ));
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn cfg_test_brace_tracking_survives_literal_braces_in_gated_items() {
    // Pre-hardening, the stray `}` in the string and the `{` in the char
    // literal both desynced the gated-item skip. Normalization removes both.
    let hits = scan_snippet(concat!(
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    fn g() -> String {\n",
        "        let open = '{';\n",
        "        format!(\"}}\")\n",
        "    }\n",
        "    fn h(pool: &P) {\n",
        "        let tx = pool.begin();\n",
        "    }\n",
        "}\n",
        "async fn prod(pool: &P) {\n",
        "    let tx = begin_immediate_tx(pool).await;\n",
        "}\n",
    ));
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn normalizer_neutralizes_char_literals_and_lifetimes() {
    let n = normalize_source(
        "fn f<'a>(x: &'a str) { let c = '{'; let q = '\"'; let e = '\\''; let s = \"after\"; }",
    );
    assert_eq!(n.matches('{').count(), 1, "{n}");
    assert_eq!(n.matches('}').count(), 1, "{n}");
    assert!(n.contains("let s"), "{n}"); // '\"' must not open a phantom string
    assert!(n.contains("<'a>"), "{n}"); // lifetimes survive as code
}

#[test]
fn normalizer_handles_raw_strings_nested_comments_and_keeps_lines() {
    let n = normalize_source(concat!(
        "let a = r#\"pool.begin() \"quoted\" {\"#;\n",
        "/* outer /* inner pool.begin() */ still comment */\n",
        "let b = br\"pool.begin()\";\n",
        "let c = 1;\n",
    ));
    assert!(!n.contains("begin"), "{n}");
    assert!(n.contains("let c = 1;"), "{n}");
    assert_eq!(n.lines().count(), 4, "{n}");
}

#[test]
fn normalizer_preserves_only_the_begin_immediate_literal() {
    let n = normalize_source(
        "pool.begin_with(\"BEGIN IMMEDIATE\"); pool.begin_with(\"BEGIN\"); let x = \"BEGIN IMMEDIATE\";",
    );
    assert!(n.contains("begin_with(\"BEGIN IMMEDIATE\")"), "{n}");
    assert!(!n.contains("(\"BEGIN\")"), "{n}");
}

#[test]
fn normalizer_handles_zero_hash_raw_strings() {
    // #930 round 2: hash count = 0 (`r"…"`/`br"…"` have no escapes) ends at the next `"`, no panics.
    let n = normalize_source(
        r####"let a = r""; let b = r"abc"; let c = br"x";
let d = r#""#; let e = r##"a"#b"##;"####,
    );
    let want = "let a = \"s\"; let b = \"s\"; let c = \"s\";\nlet d = \"s\"; let e = \"s\";";
    assert_eq!(n, want);
}

#[test]
fn scanner_ignores_begin_inside_zero_hash_raw_string() {
    // Canary: `.begin()` inside r"…" must neither flag nor panic.
    let hits = scan_snippet("fn f() {\n    let re = r\"\\d+.begin()\";\n}\n");
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn cfg_gate_matches_standalone_test_token_only() {
    assert!(is_test_gated_cfg("#[cfg(test)]"));
    assert!(is_test_gated_cfg("#[cfg(all(test, feature = \"x\"))]"));
    assert!(!is_test_gated_cfg("#[cfg(feature = \"test-utils\")]"));
    assert!(!is_test_gated_cfg("#[cfg(feature = \"testing\")]"));
    assert!(!is_test_gated_cfg("#[cfg(not(test))]"));
    assert!(!is_test_gated_cfg("#[cfg(not(any(test)))]"));
    assert!(!is_test_gated_cfg("#[cfg(any(test, not (unix)))]"));
    // r5: feature "test" is production; normalize_source blanks it to "s" first.
    let feat_test = normalize_source("#[cfg(feature = \"test\")]");
    assert!(!is_test_gated_cfg(&feat_test));
    // End-to-end: the `all(test, …)`-gated begin is skipped; the feature-gated one flags.
    let hits = scan_snippet(concat!(
        "#[cfg(all(test, feature = \"fixtures\"))]\n",
        "mod gated {\n",
        "    fn g(pool: &P) { let tx = pool.begin(); }\n",
        "}\n",
        "#[cfg(feature = \"test-utils\")]\n",
        "mod utils {\n",
        "    fn h(pool: &P) { let tx = pool.begin(); }\n",
        "}\n",
    ));
    let lines: Vec<usize> = hits.iter().map(|(l, _)| *l).collect();
    assert_eq!(lines, vec![7], "{hits:?}");
}
