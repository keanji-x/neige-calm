#!/usr/bin/env bash

set -euo pipefail

# #1252 S3′ PR-B — drift detector for the append seam.
#
# ---------------------------------------------------------------------------
# WHAT THIS IS, AND — MORE IMPORTANTLY — WHAT IT IS NOT
# ---------------------------------------------------------------------------
#
# S3′ has two halves and they guard two different statements. Do not let this
# file be described as proving the first one.
#
# **The compile-time half (not this file).** `SqlxRepo::event_append_in_tx` is a
# private inherent method whose `(actor, scope, event)` triple was replaced by a
# `gated::Authorized` capability with three private fields. `rustc` therefore
# decides, and the statement it decides is narrow and exact:
#
#     in SAFE code, no path reaches THAT APPENDER without a gate decision on the
#     very triple it inserts.
#
# It is not "the events table cannot be written" — `RepoEventWrite::write_in_tx`
# still hands out a bare `Transaction` and `SqlxRepo::pool` still hands out the
# pool — and it is not "in all code": `calm-truth` cannot carry
# `#![forbid(unsafe_code)]` (its tests need `unsafe` for `std::env::set_var`), so
# a `transmute` into `Authorized` compiles. The evidence for that half is two
# executable artifacts, neither of which is a text scan:
#
#   * `mod append_seam_escape_probe` in `src/db/sqlite/events.rs`, four
#     `#[cfg(feature)]` samples, one per bypass, asserted by CI down to the
#     diagnostic (E0616 / E0451 / E0451 / E0061);
#   * `crates/calm-truth/tests/append_seam_trybuild.rs`, the cross-crate half
#     (E0603 / E0624), which `trybuild` can carry and the in-crate half cannot.
#
# **This file is the drift detector.** It is a text scan over three files and
# one repository census. Its threat model is the one #1300 and #1318 wrote down
# for themselves: it catches somebody widening the seam *without knowing it
# exists*. It does not catch somebody working around it, and it proves nothing
# about what `rustc` compiles. The KNOWN GAPS section says how it is beaten.
#
# It does carry one thing on its own, because no type does: the residual gap the
# compile-time half explicitly leaves open — a raw `INSERT INTO events` written
# somewhere else against a bare transaction (rule S1).
#
# ---------------------------------------------------------------------------
# KNOWN GAPS — declared, not chased
# ---------------------------------------------------------------------------
#
#   G1 **Proc-macros and `build.rs`.** A derive or attribute macro expands to
#      whatever it likes, including a submodule of `events` that forges nothing
#      but reaches the appender. There is no `ident!` for a rule to see. Unlike
#      `report_write_boundary.sh` this gate cannot answer with a macro
#      allowlist, because `events.rs` legitimately invokes `sqlx::query!`-family
#      and `format!`-family macros throughout.
#
#   G2 **Anything needing a lexer.** The comment stripper blanks whole-line
#      `//` and nothing else; it does not know what a string literal is.
#      `events.rs` contains raw SQL strings by construction, so — unlike the
#      report-write gate — raw strings cannot simply be banned here. Rule E0b
#      bans raw *identifiers* (`r#name`) while allowing raw *strings* (`r#"`),
#      which is the narrowest form that leaves the file writable.
#
#   G3 **S1 is a census, not a classifier.** It pins WHERE the string
#      `INSERT INTO events` occurs and how often. It does not evaluate `#[cfg]`,
#      so it cannot tell a test-only insert from a production one; the claim
#      that today's occurrences outside `events.rs` are all prose or
#      `#[cfg(test)]` was checked by hand when the baseline was pinned (see the
#      annotations on EXPECTED_INSERTS) and is re-checked by a human whenever
#      the baseline moves. It also only sees this one spelling — a query built
#      with `QueryBuilder`, or the table name in a variable, is invisible.
#
#   G4 **E5 compares binding NAMES, not transactions.** Two distinct
#      transactions both bound to a local called `tx`, in two different
#      functions, read identical to this rule. It is a text check and is
#      documented as one below.
#
# Closing G1–G4 means parsing Rust. If this boundary ever needs an adversarial
# guarantee beyond what the capability type already gives, the answer is another
# compile-time construction, not a longer scan.
#
# ---------------------------------------------------------------------------
# WHAT IT CHECKS
# ---------------------------------------------------------------------------
#
# Over `events.rs` (the file the seam lives in):
#
#   E0a no block comment — a line-oriented stripper cannot see through one.
#   E0b no raw identifier (`r#name`). `r#event_append_in_tx` *is*
#       `event_append_in_tx` to rustc and not to a rule matching on the name.
#       Raw strings (`r#"…"#`) are allowed: the SQL needs them.
#   E0c no `#[path]`, no `include!`, no OUT-OF-LINE `mod name;`. This is the
#       rule that keeps "the caller set of the private appender" equal to one
#       file. Note it is out-of-line modules that are banned, not modules: the
#       file legitimately declares three INLINE modules (`gated`,
#       `append_probe`, `append_seam_escape_probe`), and an inline module is
#       still in front of the reviewer reading this file. Their set is pinned
#       by E1 instead.
#   E1  the inline module set is exactly the pinned three. A fourth inline
#       module is a descendant of `events` and can therefore reach the private
#       appender — legitimate, and it has to be reviewed as such.
#   E2  the exported (column-0 `pub fn`) entry set is exactly the two public
#       appenders.
#   E3  each of those two entries has exactly its pinned signature, flattened.
#       This is the "a `gate: &G` / `PermissiveGate` parameter must never grow
#       back" rule; it is written as a whole-signature pin rather than as a
#       search for `gate:` because the search only catches the spelling
#       somebody already thought of.
#   E4  `Authorized`'s field block and its `impl` block are exactly their
#       pinned text: three private fields, three by-value read-only accessors.
#       Adding `pub` to a field, a setter, or a `&mut` accessor restores
#       retargeting, which is the load-bearing half of the compile-time
#       property (see the module's own header).
#   E5  every gate mint and every append in the file names the same transaction
#       binding, `tx`. The capability binds the triple but NOT the transaction —
#       `authorize(gate_tx, …)` then `event_append_in_tx(write_tx, …)` type
#       checks — and no clean type fixes that (making `Authorized` borrow the
#       transaction makes the very next line an E0499 double mutable borrow, so
#       the seam would not compile at all). So this one is pinned textually, and
#       G4 above says exactly how weak that is.
#
# Over `decision_gate.rs`:
#
#   D1  `DecisionGate`, `PermissiveGate`, `impl DecisionGate for PermissiveGate`
#       and `commit_decision` each carry `#[cfg(any(test, feature =
#       "test-helpers"))]` **in their own attribute block** (adjacency, not
#       proximity — see `attrs_above` in lib.sh). `PermissiveGate` was the only
#       production `impl DecisionGate` in the tree and it leaked a permissive
#       stub into fifteen production call sites; losing the cfg puts it back.
#
# Over the repository:
#
#   S1  the `INSERT INTO events` census equals the pinned baseline, in both
#       directions. See G3 for what this does and does not mean.

# Every pinned list below is a `sort`ed blob compared as text, so the collation
# order has to be the same on every machine. It is not, by default: E5's census
# lines differ first at `authorize|tx` vs `authorize_with_caches|tx`, and a
# UTF-8 locale ignores the `|` while the C locale compares it as a byte (`_`
# 0x5F sorts before `|` 0x7C). This gate was green on a developer box and RED on
# CI for exactly that reason, on the unmodified production file — a false RED,
# which is the more expensive kind. Pin the collation instead of guessing it.
export LC_ALL=C

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
# shellcheck source=scripts/ci/ratchets/lib.sh
. "$script_dir/lib.sh"

require_tool rg
require_tool git

EVENTS_FILE="${APPEND_SEAM_EVENTS_FILE:-crates/calm-truth/src/db/sqlite/events.rs}"
GATE_FILE="${APPEND_SEAM_DECISION_GATE_FILE:-crates/calm-truth/src/decision_gate.rs}"
SCAN_ROOT="${APPEND_SEAM_SCAN_ROOT:-.}"
require_path "$EVENTS_FILE" "$GATE_FILE" "$SCAN_ROOT"

EXPECTED_MODULES="|gated
|append_seam_escape_probe
pub|append_probe"

EXPECTED_ENTRIES="pub|append_decision_event_in_tx
pub|append_decision_events_in_tx"

EXPECTED_SIG_SINGLE="pub async fn append_decision_event_in_tx( tx: &mut Transaction<'_, Sqlite>, actor: &ActorId, scope: &EventScope, correlation: Option<&str>, event: &Event, ) -> Result<i64> {"

EXPECTED_SIG_BATCH="pub async fn append_decision_events_in_tx( tx: &mut Transaction<'_, Sqlite>, actor: &ActorId, scope: &EventScope, correlation: Option<&str>, events: &[Event], ) -> Result<Vec<i64>> {"

EXPECTED_STRUCT="pub(in crate::db::sqlite::events) struct Authorized<'a> { actor: &'a ActorId, scope: &'a EventScope, event: &'a Event, }"

EXPECTED_IMPL="impl<'a> Authorized<'a> { pub(in crate::db::sqlite::events) fn actor(&self) -> &'a ActorId { self.actor } pub(in crate::db::sqlite::events) fn scope(&self) -> &'a EventScope { self.scope } pub(in crate::db::sqlite::events) fn event(&self) -> &'a Event { self.event } }"

# `<count> <kind>|<transaction binding>`, sorted. The two `authorize` sites are
# the public appenders; the four `authorize_with_caches` sites are the
# `RepoEventWrite` wrappers; the eight appends are those six plus the
# `#[cfg(test)]` fixture replay plus the `#[cfg(feature)]` escape probe's
# deliberately-wrong call.
EXPECTED_TX_CENSUS="4 authorize_with_caches|tx
2 authorize|tx
8 event_append_in_tx|tx"

# S1 baseline: `<path>:<count-of-MATCHING-LINES>` (`grep -c` counts lines, not
# occurrences), sorted by path. Hand-classified when pinned —
# this is the annotation G3 refers to:
#
#   calm-server/src/activity_window.rs      1  inside `#[cfg(test)] mod tests`
#   calm-server/src/task_context.rs         1  inside `#[cfg(test)] mod tests`
#   calm-server/tests/cases/*               6  integration tests
#   calm-truth/src/db/mod.rs                2  PROSE only (module doc comments)
#   calm-truth/src/db/sqlite/events.rs      2  the seam itself: the one real
#                                              production INSERT, plus prose
#   calm-truth/src/db/sqlite/mod.rs         1  PROSE only (module doc comment)
#   calm-truth/src/db/sqlite/
#     proposal_withdraw_upgrade_tests.rs    1  `#[cfg(test)] mod` (mod.rs:570)
#   calm-truth/src/events_prune.rs          1  inside `#[cfg(test)] mod tests`
#   calm-truth/tests/events_since_bound.rs  4  integration tests
#
# A new line here, or a changed count, is a claim that somebody writes the
# events table outside the seam. Adding one is allowed; it has to be argued in
# front of a reviewer, in the same PR, by editing this list.
EXPECTED_INSERTS="${APPEND_SEAM_INSERT_BASELINE-crates/calm-server/src/activity_window.rs:1
crates/calm-server/src/task_context.rs:1
crates/calm-server/tests/cases/events_pruner.rs:4
crates/calm-server/tests/cases/mcp_track_report.rs:1
crates/calm-server/tests/cases/sync_engine.rs:5
crates/calm-server/tests/cases/ws_replay.rs:1
crates/calm-truth/src/db/mod.rs:2
crates/calm-truth/src/db/sqlite/events.rs:2
crates/calm-truth/src/db/sqlite/mod.rs:1
crates/calm-truth/src/db/sqlite/proposal_withdraw_upgrade_tests.rs:1
crates/calm-truth/src/events_prune.rs:1
crates/calm-truth/tests/events_since_bound.rs:4}"

failures=0
fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

# Both subject files are read through CODE/GATE_CODE: a copy with whole-line
# `//` comments blanked out, line numbers preserved. Both files name `mod`,
# `#[path]`, `include!`, `pub` and `PermissiveGate` in their own prose, so
# rules looking for those have to be blunt and the prose has to be invisible.
# Trailing comments are deliberately NOT stripped: doing so needs a lexer, and
# the report-write gate shipped a false GREEN the one time it tried (a `//`
# inside a URL truncated the line and hid a `mod`).
strip_comments() {
  awk '{ if ($0 ~ /^[[:space:]]*\/\//) print ""; else print }' "$1"
}

CODE="$(strip_comments "$EVENTS_FILE")"
GATE_CODE="$(strip_comments "$GATE_FILE")"

# --- E0a: no block comment ---------------------------------------------------
if printf '%s' "$CODE" | rg -q '/\*|\*/'; then
  fail "E0a: $EVENTS_FILE contains a block comment. This gate strips only whole-line \`//\` comments, so a \`/* */\` can hide a declaration from every rule below. Use \`//\`."
fi

# --- E0b: no raw identifier (raw strings are fine) ---------------------------
#
# `r#"` is the raw-string opener and this file is full of SQL; `r#` followed by
# an identifier character is a raw identifier, and `r#event_append_in_tx` is
# `event_append_in_tx` to rustc but not to E2/E3/E5.
if printf '%s' "$CODE" | rg -q 'r#[A-Za-z_]'; then
  fail "E0b: $EVENTS_FILE uses a raw identifier (\`r#name\`). It defeats every name-based rule below — \`r#event_append_in_tx\` *is* \`event_append_in_tx\` to rustc. Raw strings (\`r#\"…\"#\`) are allowed and are not what this matched."
fi

# --- E0c: no declaration that extends this module to another file ------------
escape_hatch="$(printf '%s' "$CODE" | rg --line-number '^\s*(pub(\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;|#\[\s*path\s*=|(^|[^[:alnum:]_])include!\s*\(' || true)"
if [ -n "$escape_hatch" ]; then
  fail "E0c: $EVENTS_FILE declares an out-of-line module / \`#[path]\` / \`include!\`. Any of them extends the private appender's caller set to source outside this file, which is the whole basis of the boundary: $escape_hatch"
fi

# --- E1: the inline module set is exactly the pinned three -------------------
actual_modules="$(
  printf '%s' "$CODE" | rg --no-line-number --replace '$1|$2' \
    '^(pub(?:\([^)]*\))?)?\s*mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{' || true
)"
if [ "$actual_modules" != "$EXPECTED_MODULES" ]; then
  fail "E1: the inline module set in $EVENTS_FILE changed. Every module declared here is a DESCENDANT of \`events\`, so it can name \`gated::Authorized\` and can call the private \`event_append_in_tx\` — adding one is a real decision and has to be reviewed as one.
expected:
$EXPECTED_MODULES
actual:
$actual_modules"
fi

# --- E2: the exported entry set is exactly the two appenders -----------------
#
# Column-0 anchored, every `pub` form, every qualifier optional and repeatable —
# the shapes that walked past the report-write gate's first revision
# (`pub(super)`, a non-`async` fn, a generic whose name is not followed by `(`,
# an extra qualifier) are all matched here for the same reason.
actual_entries="$(
  printf '%s' "$CODE" | rg --no-line-number --replace '$1|$2' \
    '^(pub(?:\([^)]*\))?)\s+(?:(?:async|unsafe|const|extern\s+"[^"]*")\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*).*$' || true
)"
if [ "$actual_entries" != "$EXPECTED_ENTRIES" ]; then
  fail "E2: the exported append-entry set changed. A third door into the seam is a real decision — update EXPECTED_ENTRIES in this gate in the same PR.
expected:
$EXPECTED_ENTRIES
actual:
$actual_entries"
fi

# --- E3: neither appender's signature changed --------------------------------
#
# flatten_signature collects from the `pub async fn NAME(` line through the
# column-0 `)` that closes the parameter list, then squeezes whitespace. Empty
# output (the entry was renamed or removed) fails the comparison, which is the
# behaviour we want: E2 will also be red, and both messages print.
flatten_signature() {
  printf '%s' "$CODE" | awk -v pat="$1" '
    $0 ~ pat        { f = 1 }
    f               { printf "%s ", $0 }
    f && /^\)/      { exit }
  ' | tr -s ' ' | sed 's/[[:space:]]*$//'
}
actual_sig_single="$(flatten_signature '^pub async fn append_decision_event_in_tx[(]')"
if [ "$actual_sig_single" != "$EXPECTED_SIG_SINGLE" ]; then
  fail "E3: \`append_decision_event_in_tx\`'s signature changed. #1252 S3′ deleted the injected \`gate: &G\` parameter from this seam; a seam you cannot pass \"no policy\" to is stronger than one whose default policy is a real gate, so growing a policy parameter back — under any name — is the thing this rule exists to stop.
expected: $EXPECTED_SIG_SINGLE
actual:   $actual_sig_single"
fi
actual_sig_batch="$(flatten_signature '^pub async fn append_decision_events_in_tx[(]')"
if [ "$actual_sig_batch" != "$EXPECTED_SIG_BATCH" ]; then
  fail "E3: \`append_decision_events_in_tx\`'s signature changed. See the note on the single-event form above — the batch entrance must not grow a policy parameter either.
expected: $EXPECTED_SIG_BATCH
actual:   $actual_sig_batch"
fi

# --- E4: the capability type keeps its shape ---------------------------------
#
# Both blocks are pinned whole rather than probed for `pub` / `set_` / `&mut`,
# because a probe only finds the spelling somebody already thought of, and the
# blocks are six lines and twelve lines. `flatten_block` collects from the
# opening line through the first line that closes at the block's own
# indentation (4 spaces — these are items inside `mod gated`).
flatten_block() {
  printf '%s' "$CODE" | awk -v pat="$1" '
    $0 ~ pat            { f = 1 }
    f                   { printf "%s ", $0 }
    f && /^    \}/      { exit }
  ' | tr -s ' ' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
}
actual_struct="$(flatten_block 'struct Authorized<')"
if [ "$actual_struct" != "$EXPECTED_STRUCT" ]; then
  fail "E4: \`Authorized\`'s field block changed. All three fields must stay private to \`gated\`: that is what makes forging one E0451 and retargeting one E0616, and retargeting is the load-bearing half (the borrows alone only stop a triple whose values were dropped).
expected: $EXPECTED_STRUCT
actual:   $actual_struct"
fi
actual_impl="$(flatten_block '^    impl<.a> Authorized<')"
if [ "$actual_impl" != "$EXPECTED_IMPL" ]; then
  fail "E4: \`Authorized\`'s inherent impl changed. The three accessors hand out the borrows read-only; a setter, a \`&mut\` accessor, or a fourth method that returns an interior mutable handle restores retargeting and the escape probe's P1 sample would start compiling.
expected: $EXPECTED_IMPL
actual:   $actual_impl"
fi

# --- E5: every mint and every append names the same transaction binding ------
#
# Read on a whitespace-flattened copy because the calls are rustfmt-wrapped: the
# binding is on the line after the `(` at four of the six mint sites.
FLAT="$(printf '%s' "$CODE" | tr '\n' ' ' | tr -s ' ')"
actual_tx_census="$(
  printf '%s' "$FLAT" | rg --only-matching --replace '$1$2|$3' \
    '(?:gated::(authorize_with_caches|authorize)|(?:Self|SqlxRepo)::(event_append_in_tx))\(\s*(?:&mut\s+)?([A-Za-z_][A-Za-z0-9_]*)' \
    | sort | uniq -c | sed 's/^ *//' || true
)"
if [ "$actual_tx_census" != "$EXPECTED_TX_CENSUS" ]; then
  fail "E5: the mint/append transaction census changed. \`Authorized\` binds the (actor, scope, event) triple but NOT the transaction, so \`authorize(gate_tx, ..)\` followed by \`event_append_in_tx(write_tx, ..)\` type checks — and \`hydrate_role_caches_from_tx\`'s safety argument is precisely that the verdict and the insert share one transaction. This is a TEXT check on binding names (see KNOWN GAPS G4), not a proof that one transaction is used.
expected:
$EXPECTED_TX_CENSUS
actual:
$actual_tx_census"
fi

# --- D1: the test-only gate abstraction keeps its cfg ------------------------
d1_subject() {
  local label="$1" pattern="$2"
  if ! printf '%s' "$GATE_CODE" | rg -q "$pattern"; then
    fail "D1: no \`$label\` declaration found in $GATE_FILE matching /$pattern/ — if it was renamed or removed, update D1 in the same PR rather than letting the rule check nothing"
  elif ! attrs_above "$GATE_CODE" "$pattern" | rg -q '^#\[cfg\(any\(test, feature = "test-helpers"\)\)\]$'; then
    fail "D1: \`$label\` does not carry \`#[cfg(any(test, feature = \"test-helpers\"))]\` in its own attribute block. \`PermissiveGate\` was the only production \`impl DecisionGate\` in the tree; it leaked an allow-everything stub into fifteen production call sites, and #1252 S3′ deleted the parameter that carried it. Without the cfg it is production code again."
  fi
}
d1_subject "trait DecisionGate" '^pub trait DecisionGate[:[:space:]]'
d1_subject "struct PermissiveGate" '^pub struct PermissiveGate[;[:space:]]'
d1_subject "impl DecisionGate for PermissiveGate" '^impl DecisionGate for PermissiveGate[[:space:]{]'
d1_subject "fn commit_decision" '^pub async fn commit_decision[<(]'

# --- S1: the events-table insert census --------------------------------------
#
# Enumerated with `git ls-files`, not `find`: the repository has sibling git
# worktrees under `.claude/worktrees/`, which are untracked here and which a
# `find` would walk into, scanning other branches' code.
#
# The pattern is case-insensitive and whitespace-tolerant so that the trivial
# respellings do not slip past; it still only sees this one way of naming the
# table (G3).
actual_inserts="$(
  git -C "$SCAN_ROOT" ls-files -z '*.rs' \
    | (cd "$SCAN_ROOT" && xargs -0 --no-run-if-empty grep -HEic 'insert[[:space:]]+into[[:space:]]+[`"]?events\b') \
    | rg -v ':0$' | sort || true
)"
if [ "$actual_inserts" != "$EXPECTED_INSERTS" ]; then
  fail "S1: the \`INSERT INTO events\` census changed. This is the ONE thing this gate carries that no type does: the compile-time half guards the appender, not the table — \`RepoEventWrite::write_in_tx\` still hands out a bare \`Transaction\` and \`SqlxRepo::pool\` still hands out the pool, so a raw insert elsewhere bypasses the seam entirely and \`rustc\` is happy. Ratchets bite in both directions; a REMOVED occurrence is red too, because a rule nobody has to update is a rule nobody reads.
expected:
$EXPECTED_INSERTS
actual:
$actual_inserts"
fi

if [ "$failures" -ne 0 ]; then
  echo "::error::append-seam boundary gate: $failures rule(s) failed"
  exit 1
fi

echo "OK: the append seam holds its pinned shapes (module set, two entrances and their signatures, the capability type, the transaction census, the test-only gate abstraction's cfg, and the events-insert census)"
