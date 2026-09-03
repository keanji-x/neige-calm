#!/usr/bin/env bash

set -euo pipefail

# #1300 — a census of every `persist_report` / `persist_report_with_shadow`
# mention that appears on a **code line** of a `*.rs` file anywhere in the
# repository (minus the two exclusions below), pinned per file.
#
# "`*.rs` file", not "compiled Rust source", is the exact set: the scan is
# `rg --type=rust`, which selects by extension. That is narrower than what
# `rustc` compiles in one direction (a module pulled in from a differently-named
# file — `#[path = "writer.inc"]` — is real Rust that this never reads; see G6)
# and wider in another (a `*.rs` file no target includes is scanned anyway).
#
# ---------------------------------------------------------------------------
# WHAT THIS IS, STATED HONESTLY
# ---------------------------------------------------------------------------
#
# This is a **drift detector over an enumeration**, not a semantic gate. Its
# whole promise is one sentence:
#
#   A code line in a scanned `*.rs` file that names one of these two symbols
#   is counted, so adding or removing one — while writing ordinary Rust, with
#   no attempt to dodge a text scanner — changes a count here, fails the gate,
#   and puts the new call site in front of a reviewer.
#
# **Threat model: unintentional drift.** Somebody adds a report writer and does
# not know this census exists. That is what this catches. It is NOT an
# adversarial gate: it is a `rg`/`awk` text scan, and a text scan cannot decide
# what the compiler compiles. "KNOWN GAPS" below lists the ways a determined
# author walks past it, and none of them is going to be closed here — see the
# end of that section for what would actually close them.
#
# It matches the **identifier**, not `identifier(`. That is deliberate and it is
# what closes the constructions a call-shaped regex walked past:
#
#   * a call whose `(` is on the next line (rustfmt does this whenever the
#     receiver chain is long);
#   * `let save = persist_report; save(..)` — the binding line names it;
#   * a path handed to a macro, `invoke!(crate::track_report::persist_report, ..)`
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
#   5. which `EditAuthor` any call site passes — see (1). This gate reports file
#      counts and nothing about attribution. What each of the three production
#      decision points actually writes is pinned by
#      `crates/calm-server/tests/cases/report_write_characterization.rs`, which
#      drives them through the real router / tool registry and asserts the
#      persisted `events.actor` and `TrackReportEdited.author`.
#
#      Read that split precisely, because "three sites, each honest" is two
#      claims with two different backings:
#
#        * *each honest* — the per-site half. Carried by the characterization
#          suite, which drives all three decision points for real.
#        * *three* — the only-three half. Nothing strong carries it. The
#          characterization suite says so in its own header: it pins the three
#          it drives and is "a description, not a guard". The only carrier is
#          this text census, and what a text census cannot see is the KNOWN
#          GAPS list below.
#
#      An earlier revision of this census tried to carry attribution too — a
#      hand-written `Role=N` column — and was defeated three separate ways by a
#      reviewer; it has been deleted.
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
# There are exactly two *path* exclusions (the `*.rs` extension filter above is
# a separate narrowing, and G6 is where it leaks):
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
# THE ONE FILTER THERE IS, AND HOW FAR IT HOLDS
# ---------------------------------------------------------------------------
#
# Matching the bare identifier over the whole tree pins 26 files, ten of which
# mention these symbols only in prose — and a census that goes red when somebody
# rewrites a doc comment trains people to bump numbers without reading them. So
# one filter exists. `code_lines` drops a line if and only if:
#
#     its first non-blank characters are `//`  AND  it contains no `*/`
#
# The filter holds for the two lexer states ordinary Rust puts a line in:
#
#   * Not inside any comment or literal. The leading `//` opens a line comment
#     that runs to end of line. Everything after it is comment. No code.
#   * Inside a block comment. The only way code can appear on this line is for
#     the block to close first, and a block closes only at `*/`. The filter
#     refuses to drop any line containing `*/`, so this line was not dropped.
#
# An earlier revision of this comment called that a *proof* that a dropped line
# can never carry code. It is not one: it enumerated two lexer states and there
# is a third. A line can begin inside a string literal, and then its leading
# `//` is data, not a comment:
#
#     let _ = r#"open
#     //"#; persist_report(..);
#
# The second line starts with `//`, contains no `*/`, is dropped — and the
# `"#` closes the raw string so the call after it executes. An ordinary
# multi-line `"..\n.."` literal does the same. This is not fixed here; see
# KNOWN GAPS.
#
# An earlier revision dropped lines starting with `*` or `/*` as well. That was
# wrong in the direction that costs the guarantee: `/* harmless */ persist_report(..)`
# is executable code whose first non-blank characters are `/*`, and `*/ persist_report(..)`
# likewise — both were silently worth zero, which meant a new, unregistered
# production call could be added to a *new* file and stay green, because reverse
# discovery finds the file but then computes 0 occurrences and skips it.
#
# The filter errs toward over-counting on purpose: a trailing
# `// ... persist_report ...` on a code line, the body of any `/* */` block, and
# the `// */ persist_report()` comment-toggle idiom are all counted. Over-counting
# costs a census edit; under-counting costs the detection.
#
# The same filter is applied — by calling this same function, not by restating
# it — before the alias rule below, so the two rules cannot disagree about what
# a comment is.
#
# ---------------------------------------------------------------------------
# KNOWN GAPS — open, and staying open
# ---------------------------------------------------------------------------
#
# Each of these lets a real call site exist while this gate is green. They are
# listed so that no reader, and no PR touching this file, mistakes the gate for
# a closed guarantee. Nothing here is a TODO.
#
#   G1. **A code line whose start is inside a string literal.** The comment
#       filter drops it (leading `//`, no `*/`) even though the literal ends
#       mid-line and real code follows. Raw strings and ordinary multi-line
#       strings both do this; see the example above.
#
#   G2. **Files `rg` does not walk by default.** This scan passes neither
#       `--hidden`, nor `--no-ignore`, nor `--follow`. So a Rust source under a
#       dot-directory, a source matched by some `.gitignore` rule, and a
#       symlinked tree are all invisible — to the counting pass *and* to the
#       reverse-discovery pass, which is the one that would otherwise notice an
#       uncensused file. Turning the three flags on trades this for scanning
#       vendored and generated trees, which is its own kind of wrong; the trade
#       has not been made.
#
#   G3. **Legitimate source under an excluded path.** `**/target/**` is
#       excluded as build output, but `mod target;`, `#[path = "target/.."]`,
#       or a Cargo `path = "target/.."` member are all legal and would be
#       compiled while unscanned. Only the `tests/fixtures/ci-ratchets`
#       exclusion is checked for reach-in (`assert_fixture_tree_is_inert`);
#       `**/target/**` is not.
#
#   G4. **Reach-in shapes `assert_fixture_tree_is_inert` cannot see.** That
#       check is itself a regex over `path =` / `include*!(` with a *literal*
#       string. `include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))`,
#       a build.rs that emits the path, or a workspace member added by glob all
#       compute the path instead of spelling it, and none of them is matched.
#       Same for a `#[path]` reaching outside the repository entirely.
#
#   G5. **Everything in the "does NOT detect" list at the top** — an existing
#       site changing its `EditAuthor`, a new wrapper's later callers, a raw
#       `UPDATE cards SET ...`.
#
#   G6. **A module compiled from a file that is not named `*.rs`.**
#       `#[path = "writer.inc"] mod writer;` is ordinary, non-adversarial Rust:
#       `writer.inc` is compiled, is not hidden, is not ignored, and is not
#       under an excluded path — so no other gap here covers it — yet
#       `rg --type=rust` selects by extension and never opens it. Both passes
#       miss it, counting and reverse discovery alike, exactly as in G2.
#
#   G7. **An equal-count swap inside one already-censused file.** The census
#       pins a *number per file*, so any edit that keeps a file's occurrence
#       count while changing what the occurrences are stays green. Concretely:
#       turn `routes/tracks.rs`'s named `use ..::persist_report;` into a glob
#       import and add a second production call in the same file — two
#       occurrences before, two after. The `why` column would then be wrong,
#       but it is prose and nothing checks it (see "THE CENSUS" below). This
#       needs no intent; one ordinary refactor does it.
#
# Why these are accepted rather than patched: every one of them is a variation
# on the same fact — a text scan cannot decide what `rustc` compiles, so each
# patch buys one construction and the next reviewer finds another. Three rounds
# of this issue's review went that way.
#
# What it would take to actually close the set is a compile-time boundary
# narrow enough that the compiler enumerates the callers. Crate visibility is
# not that boundary and must not be described as one:
# `persist_report_with_shadow` is already `pub(crate)`, and every sibling module
# in `calm-server` can still add a call the compiler will happily accept. A
# closing shape has to be narrower than the crate — a private module with a
# single forwarding entry point, or an origin-carrying token only that entry
# point can mint — so that "who may call this" is a type/visibility fact rather
# than a text census. Designing it is not this file's job; the point here is
# only that "make it non-`pub`" is not by itself a closure.
#
# ---------------------------------------------------------------------------
# THE CENSUS
# ---------------------------------------------------------------------------
#
# Format: <file> <expected-code-line-occurrences> — classification.
#
# The classification column is prose for a reviewer. Nothing is derived from it
# and nothing checks it; the numbers are the mechanical part.
#
# The three production *call sites* are the ones #1252 S1 step 2 threads
# `WriteOrigin` through. Before #1300 there were six: `seed_template_track` and
# `restamp_template_report_if_placeholder` (deleted with template seeding, S2)
# and `update_track_template` (deleted with the template editor, S1). Those three
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

# file<TAB>count<TAB>why
census=$(cat <<'CENSUS'
crates/calm-server/src/track_report.rs	3	the two definitions (`persist_report`, `persist_report_with_shadow`) plus the wrapper's own delegation from the first to the second
crates/calm-server/src/routes/tracks.rs	2	production · REST whole-document write · ActorId::User + EditAuthor::User, and here that is honest: the caller is an authenticated browser request · plus its `use` import
crates/calm-server/src/routes/track_report_blocks.rs	2	production · REST block write · ActorId::User + EditAuthor::User, same reason · plus its `use` import
crates/calm-server/src/decision_sink.rs	2	production · the single MCP agent funnel · author derived from the caller's card role via `report_op_attribution`, never hardcoded · plus its `use` import
crates/calm-server/src/report_backlinks.rs	2	#[cfg(test)] · a fixture inside `mod tests` (the attribute is at :228, the call at :291 — see the note above on why this is not filtered) · plus its `use` import
crates/calm-server/src/track_report_read.rs	2	#[cfg(test)] · same shape
crates/calm-server/tests/cases/rest_track_report.rs	3	integration test · REST report writes · import + two fixture calls
crates/calm-server/tests/cases/track_template_tracks.rs	7	integration test · import + six fixture writes: two_tracks_from_one_template_are_independent_and_identical (four edits — append, same-length rewrite, template-minted block, deletion that shortens the body — each making a different fan-out shape falsifiable), explicit_fork_report_from_is_not_overwritten, a_forged_template_key_cannot_influence_what_a_template_creates
crates/calm-server/tests/cases/track_vcs.rs	3	integration test · import + two fixture calls
crates/calm-server/tests/cases/mcp_report_links.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/task_projection_acceptance.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/track_projection_policy_patch.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/track_report_fork.rs	2	integration test · import + one fixture call
crates/calm-server/tests/scheduler.rs	2	integration test · import + one fixture call
crates/calm-server/tests/cases/mcp_assistant_report_channel.rs	1	integration test · one fully-qualified fixture call
crates/calm-server/tests/cases/mcp_assistant_tool_gate.rs	1	integration test · one fully-qualified fixture call
CENSUS
)

# The identifier itself, not `identifier(` — see the header.
pattern='\bpersist_report(_with_shadow)?\b'
# Renaming the symbol on import. Fails on sight rather than being counted.
alias_pattern='\bpersist_report(_with_shadow)?\s+as\s'

# Emits `<line-number>:<text>` for every line of "$1" that is not comment-only,
# where "comment-only" is exactly the rule stated in the header: leading `//`
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

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census drifted — update scripts/ci/ratchets/persist_report_call_sites.sh and say why in the PR"
  exit 2
fi

# Both numbers below are aggregates over the census above; neither is a literal.
census_files=0
census_occurrences=0
while IFS=$'\t' read -r file count _why; do
  [ -n "$file" ] || continue
  census_files=$((census_files + 1))
  census_occurrences=$((census_occurrences + count))
done <<<"$census"

echo "persist_report census: $census_files files, $census_occurrences code occurrences"
