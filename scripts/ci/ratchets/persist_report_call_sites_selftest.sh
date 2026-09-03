#!/usr/bin/env bash

set -euo pipefail

# #1300 — the census gate's own fixtures.
#
# A ratchet nobody has seen fail is a ratchet nobody knows works. Each case
# below builds its **own** throwaway tree and its **own** census, because the
# cases mutate the same two things and sharing either would let one case's edit
# decide the next case's verdict.
#
# Two disciplines this file has to keep, both learned the hard way:
#
#   * **Red for the stated reason.** Every red case names the substring the gate
#     must print. An earlier revision of case 5 ("uncensused file") listed
#     `other.rs` in its census but only ever created `lib.rs`, so it tripped
#     *two* rules at once — and deleting the reverse file-discovery loop from the
#     real gate left all six cases green. A red case that does not pin its
#     message cannot tell you which rule it is exercising.
#
#   * **Blind spots get pairs.** An expected-green case on its own cannot
#     distinguish "this construction is genuinely invisible" from "this fixture
#     was never scanned at all". Case 10 is such a pair: a prose-only mention
#     stays green, and moving that same text onto a code line goes red.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"

gate="$script_dir/persist_report_call_sites.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

failures=0
cases=0

# run_case <name> <green|red> <expected-substring-when-red> <census> \
#          <relpath> <content> [<relpath> <content> ...]
#
# Runs the gate against a purpose-built tree whose census is inlined into a copy
# of the gate. Prints PASS/FAIL against the expected exit class, and for red
# cases also against the message the case claims to be provoking.
run_case() {
  local name="$1" expect="$2" want_msg="$3" census="$4"
  shift 4
  cases=$((cases + 1))
  local root; root="$(mktemp -d)"
  mkdir -p "$root/scripts/ci/ratchets"
  while [ "$#" -gt 0 ]; do
    local rel="$1" body="$2"; shift 2
    case "$rel" in
      */*) mkdir -p "$root/${rel%/*}" ;;
      *) echo "::error::fixture path must be repo-relative with a directory: $rel"; exit 1 ;;
    esac
    printf '%s' "$body" > "$root/$rel"
  done
  cp "$script_dir/lib.sh" "$root/scripts/ci/ratchets/lib.sh"
  # Same gate, census swapped for this case's.
  awk -v census="$census" '
    /^CENSUS$/ && !done { print census; print "CENSUS"; done=1; next }
    /^crates\// && !done { next }
    { print }
  ' "$gate" > "$root/scripts/ci/ratchets/gate.sh"
  chmod +x "$root/scripts/ci/ratchets/gate.sh"

  set +e
  out="$("$root/scripts/ci/ratchets/gate.sh" "$root" 2>&1)"
  rc=$?
  set -e
  rm -rf "$root"

  case "$expect" in
    green)
      if [ "$rc" -eq 0 ]; then echo "  $name: GREEN as expected"
      else echo "::error::$name expected green, got exit $rc"; printf '%s\n' "$out"; failures=1; fi
      ;;
    red)
      if [ "$rc" -lt 2 ]; then
        echo "::error::$name expected exit >= 2, got $rc"; printf '%s\n' "$out"; failures=1
      # Pure-bash substring test on purpose: `printf | grep -q` closes the pipe
      # early, and under `pipefail` that turns into a spurious non-zero.
      elif [ "${out#*"$want_msg"}" = "$out" ]; then
        echo "::error::$name went red for the wrong reason — wanted '$want_msg'"
        printf '%s\n' "$out"; failures=1
      else
        echo "  $name: RED as expected (exit $rc, '$want_msg')"
      fi
      ;;
  esac
}

lib='crates/probe/src/lib.rs'

one_call='pub fn a() { persist_report(1); }
'
two_calls='pub fn a() { persist_report(1); }
pub fn b() { persist_report(2); }
'
no_calls='pub fn a() { unrelated(1); }
'
alias_call='use other::persist_report as save;
pub fn a() { save(1); }
'
pub_alias_call='pub use other::persist_report as save;
'
alias_made_literal='use other::persist_report as save;
pub fn a() { persist_report(1); }
'
one_call_plus_comment='pub fn a() { persist_report(1); }
// an unrelated edit elsewhere in an allowlisted file
pub fn unrelated() {}
'
newline_call='pub fn a() {
    crate::track_report::persist_report
        (1);
}
'
let_binding='pub fn a() {
    let save = persist_report;
    save(1);
}
'
macro_path_arg='pub fn a() {
    invoke!(crate::track_report::persist_report, 1);
}
'
prose_only='//! This module used to reach persist_report; it no longer does.
/// See `persist_report` for the writer.
// persist_report_with_shadow(..) was here before #1300.
pub fn a() { unrelated(1); }
'
prose_moved_to_code='pub fn a() { unrelated(1); } // now calls persist_report
'
# Three lines that are executable Rust whose first non-blank characters are a
# block-comment token. The filter that also dropped `*` / `/*` / `*/` scored each
# of these zero.
block_open_prefix_call='pub fn a() {
    /* harmless */ crate::track_report::persist_report(1);
}
'
block_close_prefix_call='pub fn a() {
    /* why this is fine
    */ persist_report(1);
}
'
# The comment-toggle idiom: the `*/` closes the block opened above, so the code
# after it runs even though the line starts with `//`.
toggle_idiom_call='pub fn a() {
    /*
    // */ persist_report(1);
}
'
# An ordinary doc comment that happens to quote the alias shape. Prose, not an
# import; the alias rule must not fire on it.
alias_shaped_prose='/// Do not do this:
/// use crate::track_report::persist_report as save;
// Example: use crate::track_report::persist_report_with_shadow as save2;
pub fn a() { unrelated(1); }
'
reaches_into_fixture_tree='#[path = "../../../tests/fixtures/ci-ratchets/probe/inert.rs"]
mod inert;
pub fn a() { unrelated(1); }
'

echo "persist_report census selftest:"

# 1 — an unlisted call site in a listed file fails.
run_case "1 new call site" red "expected 1 persist_report occurrence" \
  "$lib	1	fixture" "$lib" "$two_calls"

# 2 — a listed call site that vanished fails (the census rotting into a fig leaf).
run_case "2 removed call site" red "expected 1 persist_report occurrence" \
  "$lib	1	fixture" "$lib" "$no_calls"

# 3 — an unrelated edit inside an allowlisted file stays green.
run_case "3 unrelated edit" green "" \
  "$lib	1	fixture" "$lib" "$one_call_plus_comment"

# 4 — renaming the symbol on import is red on sight, in both `use` forms, and a
#     literal call in the same file is red for the ordinary counting reason.
#     4d is the pair: the same words in a doc comment must NOT fire, or the rule
#     is just noise. (The first revision of the alias rule restated the comment
#     filter as `^[^:]+:[0-9]+:^\s*(//|\*|/\*)`, whose second `^` can never
#     match, so 4d was red.)
run_case "4a alias binding" red "imported under another name" \
  "$lib	0	fixture: alias only" "$lib" "$alias_call"
run_case "4b pub use alias" red "imported under another name" \
  "$lib	0	fixture: alias only" "$lib" "$pub_alias_call"
run_case "4c same line as a literal call" red "expected 0 persist_report occurrence" \
  "$lib	0	fixture: alias only" "$lib" "$alias_made_literal"
run_case "4d alias shape quoted in prose" green "" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/docs_only.rs' "$alias_shaped_prose"

# 5 — a file outside the census carrying calls fails, so the gate is not limited
#     to the files somebody remembered to list. The census here lists only files
#     that exist and whose counts already match, so "unlisted file" is the sole
#     reason this can be red.
run_case "5 uncensused file" red "is not in the census" \
  "$lib	1	fixture" \
  "$lib" "$one_call" \
  'crates/probe/src/other.rs' "$one_call"

# 6 — a call whose `(` sits on the next line. Invisible to a `name\s*\(` regex,
#     because ripgrep matches within a line.
run_case "6 newline call" red "expected 0 persist_report occurrence" \
  "$lib	0	fixture: no calls expected" "$lib" "$newline_call"

# 7 — the function value bound to a local, then called through the binding.
run_case "7 let-bound function value" red "expected 0 persist_report occurrence" \
  "$lib	0	fixture: no calls expected" "$lib" "$let_binding"

# 8 — the path handed to a macro as an argument.
run_case "8 path as macro argument" red "expected 0 persist_report occurrence" \
  "$lib	0	fixture: no calls expected" "$lib" "$macro_path_arg"

# 9 — a module under `crates/` but outside `src/`, the shape `#[path = "..."]`
#     produces. Invisible while discovery was `crates/*/src`.
run_case "9 module outside src/" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/report_writer.rs' "$one_call"

# 10 — the comment filter, as a pair: prose mentions in an unlisted file stay
#      green, and the same words on a code line go red.
run_case "10a prose-only mention" green "" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/docs_only.rs' "$prose_only"
run_case "10b same words on a code line" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/docs_only.rs' "$prose_moved_to_code"

# 11 — a census entry pointing at a file that no longer exists.
run_case "11 stale census entry" red "which does not exist" \
  "crates/probe/src/gone.rs	1	fixture" "$lib" "$no_calls"

# 12 — executable code whose first non-blank characters are a block-comment
#      token. Each of these is a real call in a file the census does not list;
#      while the filter also dropped `*` / `/*` / `*/`-leading lines they scored
#      zero, the reverse-discovery loop skipped the file for having no
#      occurrences, and an unregistered production call was green.
run_case "12a /* */ prefix on a call line" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/hidden.rs' "$block_open_prefix_call"
run_case "12b */ closing a block, then a call" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/hidden.rs' "$block_close_prefix_call"
run_case "12c // */ comment-toggle idiom" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/src/hidden.rs' "$toggle_idiom_call"

# 13 — a compiled Rust source outside `crates/`. `crates/calm-server/Cargo.toml`
#      registers `../../plugins/git-forge/main.rs` as a `[[bin]]`, so this is the
#      real repository shape, not a hypothetical.
run_case "13 source outside crates/" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'plugins/git-forge/main.rs' "$one_call"

# 14 — the `/tests/fixtures/ci-ratchets/**` exclusion, all three of its edges.
#      14a: the excluded tree really is excluded (other ratchets keep deliberately
#      broken `crates/` trees there and this gate must not read them as code).
#      14b: the exclusion is anchored at the repository root — an unanchored
#      `tests/fixtures/**` would also have dropped the compiled `[[bin]]` stubs
#      that live under `crates/*/tests/fixtures/`.
#      14c: the exclusion is justified by "nothing compiles it", so a scanned
#      source that reaches into it is red.
run_case "14a inert fixture tree ignored" green "" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'tests/fixtures/ci-ratchets/probe/crates/example/src/lib.rs' "$one_call"
run_case "14b compiled crate-local tests/fixtures still scanned" red "is not in the census" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$no_calls" \
  'crates/probe/tests/fixtures/stub-plugin/main.rs' "$one_call"
run_case "14c compiled source reaches into the inert tree" red "excluded from this scan because nothing compiles it" \
  "$lib	0	fixture: no calls expected" \
  "$lib" "$reaches_into_fixture_tree"

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census selftest failed"
  exit 2
fi
echo "persist_report census selftest: $cases cases, all as expected"
