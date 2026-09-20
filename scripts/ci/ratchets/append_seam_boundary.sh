#!/usr/bin/env bash

set -euo pipefail

# Drift detector for the append seam: a text scan over `events.rs`, `decision_gate.rs` and one repository census. It catches somebody widening the seam without knowing it exists; it does not catch a workaround and proves nothing about what `rustc` compiles (the compile-time half is the `Authorized` capability, the escape probe and the trybuild test).
# Known gaps: proc-macros/`build.rs`; anything needing a lexer (only whole-line `//` comments are stripped, raw strings must stay legal); S1 is a census, not a classifier; E5 compares binding NAMES, not transactions.

# Every pinned list is a `sort`ed blob compared as text; a UTF-8 locale ignores `|` while C compares it as a byte, which made this gate RED on CI on the unmodified file.
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

# `<count> <kind>|<transaction binding>`, sorted. Two public appenders, four `RepoEventWrite` wrappers, and eight appends (those six plus the test fixture replay and the escape probe's deliberately-wrong call).
EXPECTED_TX_CENSUS="4 authorize_with_caches|tx
2 authorize|tx
8 event_append_in_tx|tx"

# S1 baseline: `<path>:<count-of-MATCHING-LINES>` (`grep -c` counts lines), sorted by path. Occurrences outside `events.rs` are prose or `#[cfg(test)]`, checked by hand when pinned.
# A new line or a changed count is a claim that somebody writes the events table outside the seam; adding one has to be argued in the same PR by editing this list.
EXPECTED_INSERTS="${APPEND_SEAM_INSERT_BASELINE-crates/calm-server/src/activity_window.rs:1
crates/calm-server/src/task_context.rs:1
crates/calm-server/tests/cases/briefing_in_mint_tx.rs:1
crates/calm-server/tests/cases/events_pruner.rs:4
crates/calm-server/tests/cases/mcp_track_report.rs:1
crates/calm-server/tests/cases/migration_0094_worker_session_id.rs:1
crates/calm-server/tests/cases/rest_isolated_task_report.rs:1
crates/calm-server/tests/cases/sync_engine.rs:3
crates/calm-server/tests/cases/ws_replay.rs:1
crates/calm-truth/src/db/sqlite/events.rs:1
crates/calm-truth/src/db/sqlite/proposal_withdraw_upgrade_tests.rs:1
crates/calm-truth/src/events_prune.rs:1
crates/calm-truth/tests/events_since_bound.rs:4}"

failures=0
fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

# Both subject files are read with whole-line `//` comments blanked (line numbers preserved): both name `mod`, `#[path]`, `include!` and `pub` in their own prose. Trailing comments are deliberately NOT stripped — that needs a lexer, and a `//` inside a URL once hid a `mod`.
strip_comments() {
  awk '{ if ($0 ~ /^[[:space:]]*\/\//) print ""; else print }' "$1"
}

CODE="$(strip_comments "$EVENTS_FILE")"
GATE_CODE="$(strip_comments "$GATE_FILE")"

# Every rule feeds these blobs to its matcher through a HERE-STRING, never `printf | rg`: with `pipefail`, an early-exiting reader (`rg -q`, the `awk` helpers) kills `printf` with SIGPIPE and 141 becomes a timing-dependent FALSE RED. Readers that consume to EOF are left as pipelines.

# E0a: no block comment
if rg -q '/\*|\*/' <<<"$CODE"; then
  fail "E0a: $EVENTS_FILE contains a block comment. This gate strips only whole-line \`//\` comments, so a \`/* */\` can hide a declaration from every rule below. Use \`//\`."
fi

# E0b: no raw identifier (raw strings are fine). `r#"` opens a raw string; `r#` followed by an identifier character is a raw identifier, invisible to E2/E3/E5.
if rg -q 'r#[A-Za-z_]' <<<"$CODE"; then
  fail "E0b: $EVENTS_FILE uses a raw identifier (\`r#name\`). It defeats every name-based rule below — \`r#event_append_in_tx\` *is* \`event_append_in_tx\` to rustc. Raw strings (\`r#\"…\"#\`) are allowed and are not what this matched."
fi

# E0c: no declaration that extends this module to another file
escape_hatch="$(printf '%s' "$CODE" | rg --line-number '^\s*(pub(\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;|#\[\s*path\s*=|(^|[^[:alnum:]_])include!\s*\(' || true)"
if [ -n "$escape_hatch" ]; then
  fail "E0c: $EVENTS_FILE declares an out-of-line module / \`#[path]\` / \`include!\`. Any of them extends the private appender's caller set to source outside this file, which is the whole basis of the boundary: $escape_hatch"
fi

# E1: the inline module set is exactly the pinned three
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

# E2: the exported entry set is exactly the two appenders. Column-0 anchored, every `pub` form, every qualifier optional and repeatable.
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

# E3: neither appender's signature changed
flatten_signature() {
  awk -v pat="$1" '
    $0 ~ pat        { f = 1 }
    f               { printf "%s ", $0 }
    f && /^\)/      { exit }
  ' <<<"$CODE" | tr -s ' ' | sed 's/[[:space:]]*$//'
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

# E4: the capability type keeps its shape. Both blocks are pinned whole, since a probe only finds the spelling somebody already thought of; `flatten_block` collects from the opening line through the first line closing at the block's own 4-space indentation.
flatten_block() {
  awk -v pat="$1" '
    $0 ~ pat            { f = 1 }
    f                   { printf "%s ", $0 }
    f && /^    \}/      { exit }
  ' <<<"$CODE" | tr -s ' ' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
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

# E5: every mint and every append names the same transaction binding. Read on a whitespace-flattened copy because the calls are rustfmt-wrapped.
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

# D1: the test-only gate abstraction keeps its cfg
d1_subject() {
  local label="$1" pattern="$2" attrs rc
  if ! rg -q "$pattern" <<<"$GATE_CODE"; then
    fail "D1: no \`$label\` declaration found in $GATE_FILE matching /$pattern/ — if it was renamed or removed, update D1 in the same PR rather than letting the rule check nothing"
    return
  fi
  # Read into a variable, not a pipeline fed by an early-exiting reader. Any non-zero status is a gate malfunction, reported distinctly from the D1 verdict.
  set +e
  attrs="$(attrs_above "$GATE_CODE" "$pattern")"
  rc=$?
  set -e
  if [ "$rc" -ne 0 ]; then
    fail "D1: reading the attribute block above /$pattern/ in $GATE_FILE failed (status $rc) — this is a gate malfunction, not a verdict on \`$label\`"
    return
  fi
  if ! rg -q '^#\[cfg\(any\(test, feature = "test-helpers"\)\)\]$' <<<"$attrs"; then
    fail "D1: \`$label\` does not carry \`#[cfg(any(test, feature = \"test-helpers\"))]\` in its own attribute block. \`PermissiveGate\` was the only production \`impl DecisionGate\` in the tree; it leaked an allow-everything stub into fifteen production call sites, and #1252 S3′ deleted the parameter that carried it. Without the cfg it is production code again."
  fi
}
d1_subject "trait DecisionGate" '^pub trait DecisionGate[:[:space:]]'
d1_subject "struct PermissiveGate" '^pub struct PermissiveGate[;[:space:]]'
d1_subject "impl DecisionGate for PermissiveGate" '^impl DecisionGate for PermissiveGate[[:space:]{]'
d1_subject "fn commit_decision" '^pub async fn commit_decision[<(]'

# S1: the events-table insert census. Enumerated with `git ls-files`, not `find`: sibling worktrees under `.claude/worktrees/` are untracked and a `find` would scan other branches' code.
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
