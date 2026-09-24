#!/usr/bin/env bash

set -euo pipefail

# Drift detector for the track-report write boundary, checked over the one file that defines it. The proof is Rust privacy: `track_report::write::persist` is a private `fn`, so only `mod write` and its descendants can name it. This text scan only keeps that subtree equal to one file, as far as a text scan can tell; it catches somebody adding a writer without knowing the boundary exists, not somebody working around it.
# Known gaps (each compiles and stays GREEN): attribute/derive proc-macros and `build.rs`; macro names the regex does not shape-match; `use … as` split across lines; anything needing a lexer. Closing them means parsing Rust.
# It does not check that no other code writes the `cards` row directly, who calls the production entries or with what, or builds with `--features fixtures`.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
# shellcheck source=scripts/ci/ratchets/lib.sh
. "$script_dir/lib.sh"

require_tool rg

BOUNDARY_FILE="${REPORT_WRITE_BOUNDARY_FILE:-crates/calm-server/src/track_report/write.rs}"
require_path "$BOUNDARY_FILE"

# The pinned entry set, one `visibility|name` per line; the `pub` one is test-only and R4 additionally requires its cfg.
EXPECTED_ENTRIES="pub(crate)|rest_user_replace
pub(crate)|rest_user_block_op
pub(crate)|rest_user_start
pub(crate)|agent_report_op
pub(crate)|planner_dispatch
pub(crate)|planner_repair
pub(crate)|planner_replace
pub(crate)|structural_init_report_tx
pub|persist_report"

failures=0
fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

# Every rule reads CODE, a copy with whole-line `//` comments blanked (line numbers preserved): the module's own header names `mod`, `#[path]`, `include!` and `pub` in prose. `/* */` is rejected outright. Trailing comments are NOT stripped: without a lexer, a `//` inside a URL truncated a line and hid a `mod` — a false GREEN.
CODE="$(awk '{ if ($0 ~ /^[[:space:]]*\/\//) print ""; else print }' "$BOUNDARY_FILE")"

# Every rule feeds a blob to its matcher through a HERE-STRING, never `printf | rg`, and reads `attrs_above` into a variable: with `pipefail`, an early-exiting reader kills the writer with SIGPIPE and 141 becomes a timing-dependent verdict (a false RED here, a false GREEN in `if rg -q …; then fail`). Readers that consume to EOF are left as pipelines.

if rg -q '/\*|\*/' <<<"$CODE"; then
  fail "R0: $BOUNDARY_FILE contains a block comment. This gate strips only whole-line \`//\` comments, so a \`/* */\` can hide a declaration from every rule below. Use \`//\`."
fi

# Raw identifiers: `r#persist` *is* `persist` to rustc but not to a rule looking for `fn persist(`, so a cfg'd-out decoy plus a raw real one would leave R1 inspecting the decoy.
if rg -q 'r#' <<<"$CODE"; then
  fail "R0: $BOUNDARY_FILE uses \`r#\` — a raw identifier or a raw string. Both defeat name-based rules: \`r#persist\` *is* \`persist\` to rustc but not to a regex, and a raw string inside an attribute (\`#[doc = r#\"…\"#]\`) can carry text that looks like a second attribute to R4. Neither is needed in this file."
fi

# Aliasing, in either direction: `use std::include as format;` renames a builtin macro onto the allowlist, so a name-based allowlist cannot see it.
if rg -q '\buse\b[^;]*\bas\b' <<<"$CODE"; then
  fail "R0: $BOUNDARY_FILE contains a \`use … as …\` alias. Renaming an item — a macro especially — makes every name-based rule below inspect the wrong name."
fi

# An `impl` block can carry a `pub(crate)` associated method that reaches `persist` while sitting indented, below R3's column-0 anchor.
if rg -q '^\s*(impl|(pub(\([^)]*\))?\s+)?trait)\b' <<<"$CODE"; then
  fail "R0: $BOUNDARY_FILE declares an \`impl\` block or a \`trait\`. Both can carry a method that reaches \`persist\` while sitting indented, below R3's column-0 anchor — a \`trait\` with a *default* method is the sharper one, because a sibling implements it with an empty block and then calls the default. If one is genuinely needed, this gate has to grow a rule for it first."
fi

# Macros by ALLOWLIST, not blocklist: a macro defined elsewhere and invoked here expands inside this module (`super::door!()` expanding to `pub(crate) mod smuggled { … }` compiles and stayed GREEN under a blocklist). Every invocation must be on the list, today `format!` alone.
macro_uses="$(
  printf '%s' "$CODE" \
    | rg --line-number --only-matching '(?:[A-Za-z_][A-Za-z0-9_]*::)*[a-z_][a-z0-9_]*!' \
    | rg -v ':format!$' || true
)"
if [ -n "$macro_uses" ]; then
  fail "R0: $BOUNDARY_FILE invokes or defines a macro outside the allowlist (\`format!\`). A macro can expand to a submodule or to an entry point that appears nowhere literally, so neither R2 nor R3 can see it — and a macro defined in another file expands inside this module all the same: $macro_uses"
fi

# R1: the writer stays private. Anchored at column 0 to keep the rule off the indented call sites inside the entry bodies.
writer_decl="$(printf '%s' "$CODE" | rg --no-line-number '^[[:alnum:]_()[:space:]]*\bfn persist\(' || true)"
if [ -z "$writer_decl" ]; then
  fail "R1: no top-level \`fn persist(\` declaration found in $BOUNDARY_FILE — the boundary this gate defends is not there, so every other rule below is checking nothing"
elif [ "$(printf '%s\n' "$writer_decl" | wc -l)" -ne 1 ]; then
  fail "R1: expected exactly one top-level \`fn persist(\` in $BOUNDARY_FILE, found: $writer_decl"
elif rg -q '\bpub\b' <<<"$writer_decl"; then
  fail "R1: the writer is declared \`pub\` — that reopens the boundary to the whole crate and rustc will not complain: $writer_decl"
fi

# R1b: the writer must not be `#[cfg]`-conditional — a cfg'd-out `persist` is a decoy or a boundary that exists only in some builds.
writer_attrs="$(attrs_above "$CODE" '^[a-zA-Z_ ()]*fn persist[(]')"
if [ -n "$writer_decl" ] && rg -q '#\[[[:space:]]*cfg' <<<"$writer_attrs"; then
  fail "R1b: the writer carries a \`#[cfg]\` attribute. The boundary must exist in every build, and a cfg'd \`persist\` lets a second, differently-gated one sit beside it."
fi

# R2: no declaration that extends this module to another file
escape_hatch="$(printf '%s' "$CODE" | rg --line-number '\bmod\b|#\[\s*path\s*=|(^|[^[:alnum:]_])include!\s*\(' || true)"
if [ -n "$escape_hatch" ]; then
  fail "R2: $BOUNDARY_FILE declares a submodule / \`#[path]\` / \`include!\`, which extends the writer's caller set to source outside this file: $escape_hatch"
fi

# R2b: no re-export. `pub use persist as …;` hands the private writer out under a new name while R1 stays green.
reexport="$(printf '%s' "$CODE" | rg --line-number '^\s*pub(\([^)]*\))?\s+use\b' || true)"
if [ -n "$reexport" ]; then
  fail "R2b: $BOUNDARY_FILE re-exports something. A \`pub use\` can hand out \`persist\` under another name while its own declaration stays private: $reexport"
fi

# R3: the exported entry set is exactly the pinned one. Every `pub` form, every qualifier optional and repeatable, and the capture stops at the name: `pub(super) async fn`, a non-`async` fn, `fn fourth<T>(` and `async unsafe fn` all walked past narrower patterns. Reads CODE, not the raw file.
actual_entries="$(
  printf '%s' "$CODE" | rg --no-line-number --replace '$1|$2' \
    '^(pub(?:\([^)]*\))?)\s+(?:(?:async|unsafe|const|extern\s+"[^"]*")\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*).*$' || true
)"
if [ "$actual_entries" != "$EXPECTED_ENTRIES" ]; then
  fail "R3: the exported write-entry set changed. A new write shape is a real decision and has to be reviewed as one — update EXPECTED_ENTRIES in this gate in the same PR.
expected:
$EXPECTED_ENTRIES
actual:
$actual_entries"
fi

# R4: the test-only entry keeps its cfg, in the attribute block attached to the function — see `attrs_above` for the decoy that "nearby" admits.
test_entry_attrs="$(attrs_above "$CODE" '^pub async fn persist_report[(]')"
if ! rg -q '^pub async fn persist_report\(' <<<"$CODE"; then
  fail "R4: no \`pub async fn persist_report(\` found — if the test entry was renamed or removed, update R4 and EXPECTED_ENTRIES together"
elif ! rg -q '^#\[cfg\(any\(test, feature = "fixtures"\)\)\]$' <<<"$test_entry_attrs"; then
  fail "R4: the test-only \`persist_report\` entry does not carry \`#[cfg(any(test, feature = \"fixtures\"))]\` in its own attribute block — without it, production builds get a \`pub\` writer that takes a caller-chosen EditAuthor"
fi

if [ "$failures" -ne 0 ]; then
  echo "::error::report-write boundary gate: $failures rule(s) failed"
  exit 1
fi

echo "OK: the track-report write boundary in $BOUNDARY_FILE holds its four pinned shapes (private writer, no module escape hatch, $(printf '%s\n' "$EXPECTED_ENTRIES" | wc -l) exported entries, test entry cfg-gated)"
