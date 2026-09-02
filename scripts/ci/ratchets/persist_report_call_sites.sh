#!/usr/bin/env bash

set -euo pipefail

# #1300 — a census of every `persist_report` / `persist_report_with_shadow`
# occurrence under `crates/*/src`, pinned per file.
#
# ---------------------------------------------------------------------------
# WHAT THIS IS, STATED HONESTLY
# ---------------------------------------------------------------------------
#
# This is a **drift detector over an enumeration**, not a semantic gate. Its
# whole promise is one sentence:
#
#   Adding or removing a same-name literal direct call changes a count here and
#   fails the gate, so the allowlist has to be edited and a reviewer sees which
#   author the new call site passes.
#
# It does NOT detect, and no wording in this file or in a PR that touches it may
# claim otherwise:
#
#   1. an alias binding — `use ...::persist_report as save; save(..)`, or
#      `let p = persist_report; p(..)`. `persist_report_call_sites_selftest.sh`
#      carries a *paired* fixture for this: the alias form stays green, and
#      rewriting only that line into a literal call goes red. The pair is what
#      makes the blind spot a measured fact rather than a claim — a lone
#      "expected green" case cannot tell a blind spot apart from a fixture the
#      scan never reached.
#   2. an already-listed call site changing which `EditAuthor` it passes.
#   3. a new wrapper that delegates to one of these. The first such wrapper
#      fails here, but once the wrapper itself is in the allowlist, every later
#      production call *of the wrapper* passes silently.
#   4. a bare `UPDATE cards SET payload=..., body_crdt=...` inside any
#      `write_with_*_typed` closure. #1252 §3 P2 records why that boundary has
#      no local solution; it is declared, not eliminated.
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
# So nothing is filtered. Every occurrence is counted and every one is
# classified in the table below, test occurrences included. A count that moves
# for any reason lands on a human.
#
# ---------------------------------------------------------------------------
# THE CENSUS
# ---------------------------------------------------------------------------
#
# Format: <file> <expected-count> — classification of each occurrence.
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

# file<TAB>count<TAB>why
census=$(cat <<'CENSUS'
crates/calm-server/src/wave_report.rs	3	the two definitions (`persist_report`, `persist_report_with_shadow`) plus the wrapper's own delegation from the first to the second
crates/calm-server/src/routes/waves.rs	1	production · REST whole-document write · ActorId::User + EditAuthor::User, and here that is honest: the caller is an authenticated browser request
crates/calm-server/src/routes/wave_report_blocks.rs	1	production · REST block write · ActorId::User + EditAuthor::User, same reason
crates/calm-server/src/decision_sink.rs	1	production · the single MCP agent funnel · author derived from the caller's card role, never hardcoded
crates/calm-server/src/report_backlinks.rs	1	#[cfg(test)] · a fixture inside `mod tests` (the attribute is ~60 lines above the call — see the note above on why this is not filtered)
crates/calm-server/src/wave_report_read.rs	1	#[cfg(test)] · same shape
CENSUS
)

pattern='\bpersist_report(_with_shadow)?\s*\('
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
  actual=$(rg --no-heading --count-matches "$pattern" "$file" 2>/dev/null || echo 0)
  if [ "$actual" != "$count" ]; then
    echo "::error::$file: expected $count persist_report occurrence(s), found $actual"
    rg --no-heading --line-number "$pattern" "$file" || true
    failures=1
  fi
done <<<"$census"

# The other direction: a file the census does not mention must not contain any.
# Without this the gate would only guard the files somebody already thought of,
# which is the failure mode a census exists to avoid.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  if [ -z "${expected[$file]+set}" ]; then
    echo "::error::$file contains persist_report call(s) and is not in the census"
    rg --no-heading --line-number "$pattern" "$file" || true
    failures=1
  fi
done < <(rg --type=rust --files-with-matches "$pattern" crates/*/src 2>/dev/null || true)

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census drifted — update scripts/ci/ratchets/persist_report_call_sites.sh and say why in the PR"
  exit 2
fi

echo "persist_report census: 6 files, 8 occurrences, 3 production call sites (RestUser x2, Agent x1)"
