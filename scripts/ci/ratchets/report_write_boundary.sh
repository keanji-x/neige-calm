#!/usr/bin/env bash

set -euo pipefail

# Text drift detector, not a security guarantee: keep persist private and its module in one file.
# R0 was removed as the scan scope narrowed; its macro allowlist also guarded R2 against
# macro-generated modules. Removing it accepts that text-scan blind spot.
# Entry coverage: behavior tests (REST user attribution, agent role refusal, MCP Planner
# attribution); syn source-contract tests in fork_guard_exemption_invariant.rs (structural init).
# Removing the R3 exported-name census leaves new write entries without a static reminder;
# review must catch them.
# rustc E0425 covers R1b (cfg-hidden persist); E0364 covers R2b (private re-export).
# dead_code under -D warnings covers R4 (persist_report without its cfg) in the default-feature
# lib build. all-targets clippy enables fixtures through dev dependencies and does not cover it.
# Known out-of-scope gap: other code can write cards rows directly. Text scans also cannot
# reason about macro expansion, build.rs, strings or other constructs requiring a Rust parser.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
# shellcheck source=scripts/ci/ratchets/lib.sh
. "$script_dir/lib.sh"

require_tool rg

BOUNDARY_FILE="${REPORT_WRITE_BOUNDARY_FILE:-crates/calm-server/src/track_report/write.rs}"
require_path "$BOUNDARY_FILE"

failures=0
fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

# Blank whole-line // comments, preserving line numbers. Trailing comments are not stripped:
# without a lexer, a // inside a string could hide a declaration.
CODE="$(awk '{ if ($0 ~ /^[[:space:]]*\/\//) print ""; else print }' "$BOUNDARY_FILE")"

# R1: the writer stays private. Anchored at column 0 to keep the rule off the indented call sites inside the entry bodies.
writer_decl="$(printf '%s' "$CODE" | rg --no-line-number '^[[:alnum:]_()[:space:]]*\bfn persist\(' || true)"
if [ -z "$writer_decl" ]; then
  fail "R1: no top-level \`fn persist(\` declaration found in $BOUNDARY_FILE — the boundary this gate defends is not there, so every other rule below is checking nothing"
elif [ "$(printf '%s\n' "$writer_decl" | wc -l)" -ne 1 ]; then
  fail "R1: expected exactly one top-level \`fn persist(\` in $BOUNDARY_FILE, found: $writer_decl"
elif rg -q '\bpub\b' <<<"$writer_decl"; then
  fail "R1: the writer is declared \`pub\` — that reopens the boundary to the whole crate and rustc will not complain: $writer_decl"
fi

# R2: no declaration that extends the module to another file.
if ! no_out_of_line_module "$CODE"; then
  fail "R2: $BOUNDARY_FILE extends the writer's caller set outside this file"
fi

if [ "$failures" -ne 0 ]; then
  echo "::error::report-write boundary gate: $failures rule(s) failed"
  exit 1
fi

echo "OK: report-write drift checks (R1 private writer, R2 no out-of-line module) passed"
