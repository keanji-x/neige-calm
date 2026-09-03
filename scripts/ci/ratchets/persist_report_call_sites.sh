#!/usr/bin/env bash

set -euo pipefail

# #1300 — a census of every `persist_report` / `persist_report_with_shadow`
# mention that appears on a **code line** anywhere under `crates/`, pinned per
# file.
#
# ---------------------------------------------------------------------------
# WHAT THIS IS, STATED HONESTLY
# ---------------------------------------------------------------------------
#
# This is a **drift detector over an enumeration**, not a semantic gate. Its
# whole promise is one sentence:
#
#   Any code line under `crates/` that names one of these two symbols is
#   counted; adding or removing one changes a count here and fails the gate, so
#   the allowlist has to be edited and a reviewer sees which author the new call
#   site passes.
#
# It matches the **identifier**, not `identifier(`. That is deliberate and it is
# what closes the constructions a call-shaped regex walked past:
#
#   * a call whose `(` is on the next line (rustfmt does this whenever the
#     receiver chain is long);
#   * `let save = persist_report; save(..)` — the binding line names it;
#   * a path handed to a macro, `invoke!(crate::wave_report::persist_report, ..)`
#     — the argument names it;
#   * `use ..::persist_report as save;` / `pub use ..::persist_report as save;`
#     — see the dedicated alias rule below, which fails outright rather than
#     merely counting, because renaming this symbol is itself a thing a reviewer
#     must see.
#
# File discovery is every `*.rs` under `crates/` (ripgrep's ignore rules keep
# `target/` out, and an explicit glob does too), not `crates/*/src`. A module
# pulled in by `#[path = "../report_writer.rs"]`, an integration test under
# `crates/*/tests`, a bench under `crates/*/benches` — all of them land in the
# census now.
#
# It still does NOT detect, and no wording in this file or in a PR that touches
# it may claim otherwise:
#
#   1. an already-listed call site changing which `EditAuthor` it passes.
#   2. a new wrapper that delegates to one of these. The first such wrapper
#      fails here, but once the wrapper itself is in the allowlist, every later
#      production call *of the wrapper* passes silently.
#   3. a bare `UPDATE cards SET payload=..., body_crdt=...` inside any
#      `write_with_*_typed` closure. #1252 §3 P2 records why that boundary has
#      no local solution; it is declared, not eliminated.
#   4. a source file that lives outside `crates/`. `#[path]` can reach anywhere,
#      and the repo root cannot be scanned wholesale because
#      `tests/fixtures/ci-ratchets/**` contains deliberately-malformed `crates/`
#      trees belonging to other ratchets. Adding a Rust source root outside
#      `crates/` means widening `scan_dirs` below.
#   5. an alias split across lines (`use a::persist_report\n as save;`). The
#      identifier still gets counted — only the louder alias-specific message is
#      lost — so this degrades to "red for the ordinary reason", not to green.
#
# What turns "who is writing" into a mechanical fact is #1252 S1 step 2's
# origin-only constructors. This file is the interim, and it is a review aid.
#
# ---------------------------------------------------------------------------
# WHY IT COUNTS OCCURRENCES INSTEAD OF FILTERING OUT TESTS
# ---------------------------------------------------------------------------
#
# The obvious shape — "scan production call sites, excluding `#[cfg(test)]`" —
# cannot be built reliably in shell. Two reviewers independently produced the
# same counterexamples: in `report_backlinks.rs` the attribute is at :228 and
# the call at :291, so "skip from the attribute to EOF" would swallow any
# production call added after the test module; brace counting is defeated by
# macros and string literals; and `cfg(any(test, feature = "fixtures"))` is not
# `cfg(test)` at all.
#
# So no test filtering happens. Every code occurrence is counted and every one
# is classified in the table below, test occurrences included. A count that
# moves for any reason lands on a human.
#
# ---------------------------------------------------------------------------
# THE ONE FILTER THERE IS, AND WHY IT IS SAFE
# ---------------------------------------------------------------------------
#
# Lines whose first non-blank characters are `//`, `*` or `/*` are dropped
# before counting. Matching the bare identifier over the whole tree otherwise
# pins 26 files, ten of which mention these symbols only in prose — and a census
# that goes red when somebody rewrites a doc comment trains people to bump
# numbers without reading them.
#
# The filter is safe in the only direction that matters: a Rust *call* can never
# live on a line that begins with `//`, so nothing executable can hide behind
# it. It errs the other way instead — a trailing `// ... persist_report ...` on
# a code line, or the body of a `/* */` block whose lines happen not to start
# with `*`, is still counted. Over-counting costs a census edit; under-counting
# would cost the guarantee.
#
# ---------------------------------------------------------------------------
# THE CENSUS
# ---------------------------------------------------------------------------
#
# Format: <file> <expected-code-line-occurrences> — classification.
#
# The three production *call sites* are the ones #1252 S1 step 2 threads
# `WriteOrigin` through. Before #1300 there were six: `seed_template_wave` and
# `restamp_template_report_if_placeholder` (deleted with template seeding, S2)
# and `update_wave_template` (deleted with the template editor, S1). Those three
# passed `ActorId::User` + `EditAuthor::User` for writes no user made — the
# thing #1300 exists to remove — and their four-argument tuple was byte-identical
# to the REST user write below, so no later reader could have told them apart.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
# shellcheck source=lib.sh
. "$script_dir/lib.sh"

require_tool rg

scan_root="${1:-$script_dir/../../..}"
require_path "$scan_root"
scan_root="$(cd "$scan_root" && pwd)"
require_path "$scan_root/crates"
cd "$scan_root"

scan_dirs=(crates)

# file<TAB>count<TAB>why
census=$(cat <<'CENSUS'
crates/calm-server/src/wave_report.rs	3	the two definitions (`persist_report`, `persist_report_with_shadow`) plus the wrapper's own delegation from the first to the second
crates/calm-server/src/routes/waves.rs	2	production · REST whole-document write · ActorId::User + EditAuthor::User, and here that is honest: the caller is an authenticated browser request · plus its `use` import
crates/calm-server/src/routes/wave_report_blocks.rs	2	production · REST block write · ActorId::User + EditAuthor::User, same reason · plus its `use` import
crates/calm-server/src/decision_sink.rs	2	production · the single MCP agent funnel · author derived from the caller's card role, never hardcoded · plus its `use` import
crates/calm-server/src/report_backlinks.rs	2	#[cfg(test)] · a fixture inside `mod tests` (the attribute is at :228, the call at :291 — see the note above on why this is not filtered) · plus its `use` import
crates/calm-server/src/wave_report_read.rs	2	#[cfg(test)] · same shape
crates/calm-server/tests/cases/rest_wave_report.rs	3	integration test · REST report writes · import + two fixture calls
crates/calm-server/tests/cases/wave_template_waves.rs	4	integration test · import + three fixture writes: two_waves_from_one_template_are_independent_and_identical (the edit that makes independence falsifiable), explicit_fork_report_from_is_not_overwritten, a_forged_template_key_cannot_influence_what_a_template_creates
crates/calm-server/tests/cases/wave_vcs.rs	3	integration test · import + two fixture calls
crates/calm-server/tests/cases/mcp_report_links.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/task_projection_acceptance.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/wave_projection_policy_patch.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/wave_report_fork.rs	2	integration test · import + one fixture call
crates/calm-server/tests/scheduler.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/mcp_assistant_report_channel.rs	1	integration test · one fully-qualified fixture call
crates/calm-server/tests/cases/mcp_assistant_tool_gate.rs	1	integration test · one fully-qualified fixture call
CENSUS
)

# The identifier itself, not `identifier(` — see the header.
pattern='\bpersist_report(_with_shadow)?\b'
# Renaming the symbol on import. Fails on sight rather than being counted.
alias_pattern='\bpersist_report(_with_shadow)?\s+as\s'
# Comment-only lines, dropped before anything is counted.
comment_pattern='^\s*(//|\*|/\*)'

# Emits `<line-number>:<text>` for every line of "$1" that is not comment-only.
code_lines() {
  rg --line-number --invert-match --regexp "$comment_pattern" -- "$1" || true
}

# Occurrences of $pattern on code lines of "$1", as a bare integer.
occurrences() {
  local n
  n="$(code_lines "$1" | rg --count-matches --regexp "$pattern" || true)"
  printf '%s' "${n:-0}"
}

failures=0
declare -A expected=()

while IFS=$'\t' read -r file count _why; do
  [ -n "$file" ] || continue
  expected["$file"]="$count"
  if [ ! -f "$file" ]; then
    echo "::error::census lists $file, which does not exist — the allowlist is stale"
    failures=1
    continue
  fi
  actual="$(occurrences "$file")"
  if [ "$actual" != "$count" ]; then
    echo "::error::$file: expected $count persist_report occurrence(s) on code lines, found $actual"
    code_lines "$file" | rg --regexp "$pattern" || true
    failures=1
  fi
done <<<"$census"

# The other direction: a file the census does not mention must not contain any.
# Without this the gate would only guard the files somebody already thought of,
# which is the failure mode a census exists to avoid.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  [ "$(occurrences "$file")" != "0" ] || continue
  if [ -z "${expected[$file]+set}" ]; then
    echo "::error::$file contains persist_report call(s) and is not in the census"
    code_lines "$file" | rg --regexp "$pattern" || true
    failures=1
  fi
done < <(rg --type=rust --files-with-matches --glob '!**/target/**' \
  --regexp "$pattern" "${scan_dirs[@]}" 2>/dev/null || true)

# Aliasing the symbol on import hides every later use behind a name this census
# does not know. There is no legitimate reason to do it here, so it is red on
# sight — not merely counted.
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  echo "::error::$hit"
  echo "::error::persist_report is imported under another name — the census cannot follow the alias; import it unrenamed"
  failures=1
done < <(rg --type=rust --line-number --no-heading --glob '!**/target/**' \
  --regexp "$alias_pattern" "${scan_dirs[@]}" 2>/dev/null \
  | rg --invert-match --regexp "^[^:]+:[0-9]+:$comment_pattern" || true)

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census drifted — update scripts/ci/ratchets/persist_report_call_sites.sh and say why in the PR"
  exit 2
fi

# Derived from the census above so the summary cannot drift away from it. The
# "3 production call sites" is the pinned claim: decision_sink.rs (agent funnel),
# routes/wave_report_blocks.rs and routes/waves.rs (REST user writes).
census_files=0
census_occurrences=0
while IFS=$'\t' read -r file count _why; do
  [ -n "$file" ] || continue
  census_files=$((census_files + 1))
  census_occurrences=$((census_occurrences + count))
done <<<"$census"
echo "persist_report census: $census_files files, $census_occurrences code occurrences, 3 production call sites (RestUser x2, Agent x1)"
