#!/usr/bin/env bash

set -euo pipefail

# #1300 — a census of every `persist_report` / `persist_report_with_shadow`
# mention that appears on a **code line** anywhere in the repository's compiled
# Rust sources, pinned per file.
#
# ---------------------------------------------------------------------------
# WHAT THIS IS, STATED HONESTLY
# ---------------------------------------------------------------------------
#
# This is a **drift detector over an enumeration**, not a semantic gate. Its
# whole promise is one sentence:
#
#   Any code line in a compiled Rust source that names one of these two symbols
#   is counted; adding or removing one changes a count here and fails the gate,
#   so the allowlist has to be edited and a reviewer sees which author the new
#   call site passes.
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
#   4. an alias split across lines (`use a::persist_report\n as save;`). The
#      identifier still gets counted — only the louder alias-specific message is
#      lost — so this degrades to "red for the ordinary reason", not to green.
#   5. whether the origin role a production call site is *labelled* with in the
#      census is the role it actually passes. See "THE ORIGIN COLUMN" below for
#      exactly how far the mechanical check goes.
#
# What turns "who is writing" into a mechanical fact is #1252 S1 step 2's
# origin-only constructors. This file is the interim, and it is a review aid.
#
# ---------------------------------------------------------------------------
# WHICH FILES ARE SCANNED, AND WHY THAT SET
# ---------------------------------------------------------------------------
#
# The scan root is the whole repository, not `crates/`. Restricting it to
# `crates/` was a real hole: `crates/calm-server/Cargo.toml` registers
# `../../plugins/git-forge/main.rs` as a `[[bin]]`, so `plugins/` is compiled
# production code that a `crates/`-only scan could not see — and `persist_report`
# is `pub`, so that binary can call it. Enumerating Rust source roots by hand is
# the same mistake one directory up: the next root added to the workspace would
# be invisible again. So the default is "everything", and exclusions have to earn
# themselves one at a time.
#
# There are exactly two exclusions:
#
#   * `**/target/**` — build output, not source.
#   * `/tests/fixtures/ci-ratchets/**` — deliberately-malformed miniature
#     `crates/` trees that exist as *input data* for other ratchets' selftests.
#     Note the leading `/`: it anchors the glob at the repository root, so the
#     genuinely-compiled `crates/*/tests/fixtures/**` bins (several `[[bin]]`
#     stubs live there) stay in the scan. An unanchored `tests/fixtures/**`
#     would have silently dropped them.
#
# The second exclusion is only safe while nothing compiled reaches into that
# tree, and that is not assumed — it is checked. `assert_fixture_tree_is_inert`
# below fails the gate if any scanned Rust source or any `Cargo.toml` names a
# path under `tests/fixtures/ci-ratchets` (`#[path = ..]`, `include!`,
# `include_str!`, `include_bytes!`, a Cargo `path =` target). The day somebody
# compiles that tree, the exclusion stops being free and this gate says so.
#
# Still not covered: a `#[path]` reaching outside the repository entirely. That
# needs a filesystem the gate does not own.
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
# THE ONE FILTER THERE IS, AND WHY IT CANNOT UNDER-COUNT
# ---------------------------------------------------------------------------
#
# Matching the bare identifier over the whole tree pins 26 files, ten of which
# mention these symbols only in prose — and a census that goes red when somebody
# rewrites a doc comment trains people to bump numbers without reading them. So
# one filter exists. `code_lines` drops a line if and only if:
#
#     its first non-blank characters are `//`  AND  it contains no `*/`
#
# The claim is that a dropped line can never carry executable code. Proof, by
# the two lexer states a line can start in:
#
#   * Not inside a block comment. The leading `//` opens a line comment that
#     runs to end of line. Everything after it is comment. No code.
#   * Inside a block comment. The only way code can appear on this line is for
#     the block to close first, and a block closes only at `*/`. The filter
#     refuses to drop any line containing `*/`, so this line was not dropped.
#
# An earlier revision dropped lines starting with `*` or `/*` as well. That was
# wrong in the direction that costs the guarantee: `/* harmless */ persist_report(..)`
# is executable code whose first non-blank characters are `/*`, and `*/ persist_report(..)`
# likewise — both were silently worth zero, which meant a new, unregistered
# production call could be added to a *new* file and stay green, because reverse
# discovery finds the file but then computes 0 occurrences and skips it.
#
# The filter that remains errs the other way, on purpose: a trailing
# `// ... persist_report ...` on a code line, the body of any `/* */` block, and
# the `// */ persist_report()` comment-toggle idiom are all counted. Over-counting
# costs a census edit. Under-counting would cost the guarantee.
#
# The same filter is applied — by calling this same function, not by restating
# it — before the alias rule below, so the two rules cannot disagree about what
# a comment is.
#
# ---------------------------------------------------------------------------
# THE ORIGIN COLUMN
# ---------------------------------------------------------------------------
#
# Column 3 of the census names the production call sites in that file and the
# origin role each one writes as, `Role=N`, or `-` for none. The summary line at
# the bottom is computed from this column: the total and the per-role breakdown
# are both aggregates over it, so bumping the number in the summary is not
# possible without editing the census. (Before, that sentence was a hardcoded
# string and a fourth production call site could be added, censused correctly,
# and still print "3".)
#
# What the gate checks about column 3:
#
#   * the role name is in `origin_markers` below — an invented role is red;
#   * the declared count does not exceed that file's total occurrence count;
#   * the role's marker regex occurs on at least one code line of that file.
#     `RestUser` requires `EditAuthor::User`; `Agent` requires
#     `report_op_attribution`, the function that derives the author from the
#     caller's card role. Relabelling `decision_sink.rs` from `Agent` to
#     `RestUser` without touching the code is therefore red.
#
# What it does not check, stated so the summary line is not read as more than it
# is: dropping a call site's annotation entirely leaves the occurrence counts
# untouched and goes green with a smaller total. The printed line is therefore
# worded as what the census *declares*, which is exactly the artifact a reviewer
# is being asked to read.
#
# ---------------------------------------------------------------------------
# THE CENSUS
# ---------------------------------------------------------------------------
#
# Format: <file> <expected-code-line-occurrences> <origins> — classification.
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
require_tool awk

scan_root="${1:-$script_dir/../../..}"
require_path "$scan_root"
scan_root="$(cd "$scan_root" && pwd)"
require_path "$scan_root/crates"
cd "$scan_root"

# See "WHICH FILES ARE SCANNED". The leading `/` on the fixture glob anchors it
# at the repository root; without it, compiled `crates/*/tests/fixtures/**` bins
# would be excluded too.
scan_globs=(--glob '!**/target/**' --glob '!/tests/fixtures/ci-ratchets/**')
inert_fixture_tree='tests/fixtures/ci-ratchets'

# file<TAB>count<TAB>origins<TAB>why
census=$(cat <<'CENSUS'
crates/calm-server/src/wave_report.rs	3	-	the two definitions (`persist_report`, `persist_report_with_shadow`) plus the wrapper's own delegation from the first to the second
crates/calm-server/src/routes/waves.rs	2	RestUser=1	production · REST whole-document write · ActorId::User + EditAuthor::User, and here that is honest: the caller is an authenticated browser request · plus its `use` import
crates/calm-server/src/routes/wave_report_blocks.rs	2	RestUser=1	production · REST block write · ActorId::User + EditAuthor::User, same reason · plus its `use` import
crates/calm-server/src/decision_sink.rs	2	Agent=1	production · the single MCP agent funnel · author derived from the caller's card role via `report_op_attribution`, never hardcoded · plus its `use` import
crates/calm-server/src/report_backlinks.rs	2	-	#[cfg(test)] · a fixture inside `mod tests` (the attribute is at :228, the call at :291 — see the note above on why this is not filtered) · plus its `use` import
crates/calm-server/src/wave_report_read.rs	2	-	#[cfg(test)] · same shape
crates/calm-server/tests/cases/rest_wave_report.rs	3	-	integration test · REST report writes · import + two fixture calls
crates/calm-server/tests/cases/wave_template_waves.rs	4	-	integration test · import + three fixture writes: two_waves_from_one_template_are_independent_and_identical (the edit that makes independence falsifiable), explicit_fork_report_from_is_not_overwritten, a_forged_template_key_cannot_influence_what_a_template_creates
crates/calm-server/tests/cases/wave_vcs.rs	3	-	integration test · import + two fixture calls
crates/calm-server/tests/cases/mcp_report_links.rs	2	-	integration test · import + one fixture call
crates/calm-server/tests/cases/task_projection_acceptance.rs	2	-	integration test · import + one fixture call
crates/calm-server/tests/cases/wave_projection_policy_patch.rs	2	-	integration test · import + one fixture call
crates/calm-server/tests/cases/wave_report_fork.rs	2	-	integration test · import + one fixture call
crates/calm-server/tests/scheduler.rs	2	-	integration test · import + one fixture call
crates/calm-server/tests/cases/mcp_assistant_report_channel.rs	1	-	integration test · one fully-qualified fixture call
crates/calm-server/tests/cases/mcp_assistant_tool_gate.rs	1	-	integration test · one fully-qualified fixture call
CENSUS
)

# The identifier itself, not `identifier(` — see the header.
pattern='\bpersist_report(_with_shadow)?\b'
# Renaming the symbol on import. Fails on sight rather than being counted.
alias_pattern='\bpersist_report(_with_shadow)?\s+as\s'

# Origin roles the census may use, and the regex each one has to be able to
# point at on a code line of the file that declares it. Iteration order here is
# the order the summary breakdown prints in.
origin_roles=(RestUser Agent)
declare -A origin_markers=(
  [RestUser]='EditAuthor::User'
  [Agent]='report_op_attribution'
)

# Emits `<line-number>:<text>` for every line of "$1" that is not comment-only,
# where "comment-only" is exactly the rule proven in the header: leading `//`
# and no `*/` anywhere on the line.
code_lines() {
  awk '{
    stripped = $0
    sub(/^[ \t]*/, "", stripped)
    if (stripped ~ /^\/\// && index(stripped, "*/") == 0) next
    printf "%d:%s\n", NR, $0
  }' "$1"
}

# Occurrences of $pattern on code lines of "$1", as a bare integer.
occurrences() {
  local n
  n="$(code_lines "$1" | rg --count-matches --regexp "$pattern" || true)"
  printf '%s' "${n:-0}"
}

# Repo-relative paths of scanned Rust sources containing "$1".
sources_matching() {
  rg --type=rust --files-with-matches "${scan_globs[@]}" \
    --regexp "$1" . 2>/dev/null | sed 's|^\./||' || true
}

failures=0

# The `/tests/fixtures/ci-ratchets/**` exclusion is only sound while that tree is
# input data rather than compiled source. Check it instead of assuming it.
assert_fixture_tree_is_inert() {
  local hits
  hits="$(rg --line-number --no-heading "${scan_globs[@]}" \
    --glob '*.rs' --glob 'Cargo.toml' \
    --regexp "(path\s*=\s*|include(_str|_bytes)?!\(\s*)\"[^\"]*$inert_fixture_tree" \
    . 2>/dev/null || true)"
  [ -n "$hits" ] || return 0
  printf '%s\n' "$hits" | sed 's|^|::error::|'
  echo "::error::$inert_fixture_tree is excluded from this scan because nothing compiles it; the reference(s) above break that assumption — either drop the reference or stop excluding the tree"
  failures=1
}
assert_fixture_tree_is_inert

declare -A expected=()
declare -A declared_origins=()

while IFS=$'\t' read -r file count origins _why; do
  [ -n "$file" ] || continue
  expected["$file"]="$count"
  declared_origins["$file"]="$origins"
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
done < <(sources_matching "$pattern")

# Aliasing the symbol on import hides every later use behind a name this census
# does not know. There is no legitimate reason to do it here, so it is red on
# sight — not merely counted. The comment filter is applied by calling
# `code_lines`, the same function counting uses, so a doc comment that merely
# *quotes* an alias cannot trip this. (An earlier revision restated the filter as
# a second regex and got it wrong — `^[^:]+:[0-9]+:^\s*...` can never match — so
# every prose mention of the alias shape was a false red.)
while IFS= read -r file; do
  [ -n "$file" ] || continue
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    echo "::error::$file:$hit"
    echo "::error::persist_report is imported under another name — the census cannot follow the alias; import it unrenamed"
    failures=1
  done < <(code_lines "$file" | rg --regexp "$alias_pattern" || true)
done < <(sources_matching "$alias_pattern")

# Column 3 has to survive the checks the header promises for it before anything
# is derived from it.
declare -A origin_totals=()
for role in "${origin_roles[@]}"; do origin_totals["$role"]=0; done

for file in "${!declared_origins[@]}"; do
  origins="${declared_origins[$file]}"
  [ "$origins" != "-" ] || continue
  file_declared=0
  IFS=',' read -r -a entries <<<"$origins"
  for entry in "${entries[@]}"; do
    role="${entry%%=*}"
    n="${entry#*=}"
    if [ -z "${origin_markers[$role]+set}" ]; then
      echo "::error::$file: census declares unknown origin role '$role'; known roles: ${origin_roles[*]}"
      failures=1
      continue
    fi
    case "$n" in
      ''|*[!0-9]*)
        echo "::error::$file: origin entry '$entry' is not <Role>=<count>"
        failures=1
        continue
        ;;
    esac
    if [ ! -f "$file" ]; then continue; fi
    # Not `rg --quiet`: it closes the pipe on the first hit, awk dies of SIGPIPE
    # and `pipefail` turns a *found* marker into a failure.
    marker_hits="$(code_lines "$file" | rg --count-matches --regexp "${origin_markers[$role]}" || true)"
    if [ -z "$marker_hits" ] || [ "$marker_hits" = "0" ]; then
      echo "::error::$file: census declares origin role '$role', but its marker /${origin_markers[$role]}/ appears on no code line of that file"
      failures=1
      continue
    fi
    origin_totals["$role"]=$((origin_totals[$role] + n))
    file_declared=$((file_declared + n))
  done
  if [ -f "$file" ] && [ "$file_declared" -gt "${expected[$file]}" ]; then
    echo "::error::$file: census declares $file_declared production call site(s) but only ${expected[$file]} occurrence(s)"
    failures=1
  fi
done

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census drifted — update scripts/ci/ratchets/persist_report_call_sites.sh and say why in the PR"
  exit 2
fi

# Every number below is an aggregate over the census above; none of it is a
# literal. "declares" is the honest verb — see "THE ORIGIN COLUMN".
census_files=0
census_occurrences=0
while IFS=$'\t' read -r file count _origins _why; do
  [ -n "$file" ] || continue
  census_files=$((census_files + 1))
  census_occurrences=$((census_occurrences + count))
done <<<"$census"

origin_total=0
breakdown=""
for role in "${origin_roles[@]}"; do
  n="${origin_totals[$role]}"
  [ "$n" -gt 0 ] || continue
  origin_total=$((origin_total + n))
  [ -z "$breakdown" ] || breakdown="$breakdown, "
  breakdown="$breakdown$role x$n"
done

echo "persist_report census: $census_files files, $census_occurrences code occurrences; census declares $origin_total production call sites ($breakdown)"
