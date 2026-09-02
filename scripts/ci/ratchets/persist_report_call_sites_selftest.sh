#!/usr/bin/env bash

set -euo pipefail

# #1300 — the census gate's own fixtures.
#
# A ratchet nobody has seen fail is a ratchet nobody knows works. Each case
# below builds its **own** throwaway tree and its **own** census, because the
# cases mutate the same two things and sharing either would let one case's edit
# decide the next case's verdict.
#
# Case 4 is a pair, and the pairing is the point. `4a` asserts an alias binding
# stays green — an expected-green case on its own cannot distinguish "the alias
# form is a real blind spot" from "this fixture was never scanned at all". `4b`
# rewrites that one line into a literal call and requires red. Only 4a+4b
# together make the blind spot a measured fact.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"

gate="$script_dir/persist_report_call_sites.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

failures=0

# Runs the gate against a purpose-built tree whose census is inlined into a copy
# of the gate. Prints PASS/FAIL against the expected exit class.
run_case() {
  local name="$1" expect="$2" census="$3" source="$4"
  local root; root="$(mktemp -d)"
  mkdir -p "$root/crates/probe/src" "$root/scripts/ci/ratchets"
  printf '%s' "$source" > "$root/crates/probe/src/lib.rs"
  cp "$script_dir/lib.sh" "$root/scripts/ci/ratchets/lib.sh"
  # Same gate, census swapped for this case's.
  awk -v census="$census" '
    /^CENSUS$/ && !done { print census; print "CENSUS"; done=1; next }
    /^crates\/calm-server/ && !done { next }
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
      if [ "$rc" -ge 2 ]; then echo "  $name: RED as expected (exit $rc)"
      else echo "::error::$name expected exit >= 2, got $rc"; printf '%s\n' "$out"; failures=1; fi
      ;;
  esac
}

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
alias_made_literal='use other::persist_report as save;
pub fn a() { persist_report(1); }
'
one_call_plus_comment='pub fn a() { persist_report(1); }
// an unrelated edit elsewhere in an allowlisted file
pub fn unrelated() {}
'

echo "persist_report census selftest:"

# 1 — an unlisted call site fails.
run_case "1 new call site" red \
  "crates/probe/src/lib.rs	1	fixture" "$two_calls"

# 2 — a listed call site that vanished fails (the census rotting into a fig leaf).
run_case "2 removed call site" red \
  "crates/probe/src/lib.rs	1	fixture" "$no_calls"

# 3 — an unrelated edit inside an allowlisted file stays green.
run_case "3 unrelated edit" green \
  "crates/probe/src/lib.rs	1	fixture" "$one_call_plus_comment"

# 4a/4b — the alias blind spot, as a pair.
run_case "4a alias binding (known blind spot)" green \
  "crates/probe/src/lib.rs	0	fixture: alias only" "$alias_call"
run_case "4b same line as a literal call" red \
  "crates/probe/src/lib.rs	0	fixture: alias only" "$alias_made_literal"

# 5 — a file outside the census carrying calls fails, so the gate is not limited
#     to the files somebody remembered to list.
run_case "5 uncensused file" red \
  "crates/probe/src/other.rs	1	fixture" "$one_call"

if [ "$failures" -ne 0 ]; then
  echo "::error::persist_report census selftest failed"
  exit 2
fi
echo "persist_report census selftest: 6 cases, all as expected"
