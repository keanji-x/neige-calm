#!/usr/bin/env bash

set -euo pipefail

# Fixtures for `append_seam_boundary.sh`. Every rule has a fixture that has been watched go red, and each pins a substring of its rule's message (exit 1 alone is not evidence). One violation per red case; the substring test is bash's own `case`, not `printf | grep -qF`, which flaked roughly once in forty runs. Green fixtures are the real production files.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

gate="$script_dir/append_seam_boundary.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

real_events="$repo_root/crates/calm-truth/src/db/sqlite/events.rs"
real_gate_file="$repo_root/crates/calm-truth/src/decision_gate.rs"
for f in "$real_events" "$real_gate_file"; do
  [ -f "$f" ] || { echo "::error::production subject file missing: $f"; exit 1; }
done

failures=0
cases=0

# check <name> <green|red> <expected-substring-when-red> <output> <rc>
check() {
  local name="$1" expect="$2" want_msg="$3" output="$4" rc="$5"
  case "$expect" in
    green)
      if [ "$rc" -eq 0 ]; then
        echo "PASS [$name]: green"
      else
        echo "FAIL [$name]: expected green, got exit $rc"
        printf '%s\n' "$output"
        failures=$((failures + 1))
      fi
      ;;
    red)
      if [ "$rc" -eq 0 ]; then
        echo "FAIL [$name]: expected red, gate passed"
        failures=$((failures + 1))
      elif case "$output" in *"$want_msg"*) false ;; *) true ;; esac; then
        echo "FAIL [$name]: red, but not for the stated reason (wanted substring: $want_msg)"
        printf '%s\n' "$output"
        failures=$((failures + 1))
      else
        echo "PASS [$name]: red on \"$want_msg\""
      fi
      ;;
  esac
}

# run_file_case <name> <green|red> <msg> <events|gate> <sed-program>: copies the production file, applies the sed program, points the gate at the copy. The other subject and the census stay real.
run_file_case() {
  local name="$1" expect="$2" want_msg="$3" subject="$4" program="$5"
  cases=$((cases + 1))
  local dir; dir="$(mktemp -d)"
  local src copy env_var
  case "$subject" in
    events) src="$real_events"; copy="$dir/events.rs"; env_var=APPEND_SEAM_EVENTS_FILE ;;
    gate)   src="$real_gate_file"; copy="$dir/decision_gate.rs"; env_var=APPEND_SEAM_DECISION_GATE_FILE ;;
    *) echo "FAIL [$name]: unknown subject $subject"; failures=$((failures + 1)); rm -rf "$dir"; return ;;
  esac
  sed "$program" "$src" > "$copy"

  # Applies to GREEN cases too: a green case whose sed silently stops matching degrades into the first case and tests nothing.
  if [ -n "$program" ] && cmp -s "$copy" "$src"; then
    echo "FAIL [$name]: the mutation changed nothing — the sed program no longer matches the production file, so this case is testing the green fixture"
    failures=$((failures + 1))
    rm -rf "$dir"
    return
  fi

  local output rc
  set +e
  output="$(cd "$repo_root" && env "$env_var=$copy" "$gate" 2>&1)"
  rc=$?
  set -e
  check "$name" "$expect" "$want_msg" "$output" "$rc"
  rm -rf "$dir"
}

# The production tree, unmodified, must pass — or every red case could be red because the gate is broken in general.
run_file_case "green: production events.rs as-is" green "" events ""

# E0a / E0b / E0c: readability and the module escape hatches

run_file_case "E0a: block comment" red "E0a:" events \
  '/^use crate::track_vcs;$/a\
/* nothing to see here */'

# A raw IDENTIFIER; raw strings must stay legal, and the green case above proves the SQL still passes.
run_file_case "E0b: raw identifier" red "E0b:" events \
  '/^use crate::track_vcs;$/a\
const r#type: u8 = 0;'

run_file_case "E0c: out-of-line module" red "E0c:" events \
  '/^use crate::track_vcs;$/a\
mod helper;'

run_file_case "E0c: #[path] module" red "E0c:" events \
  '/^use crate::track_vcs;$/a\
#[path = "../elsewhere.rs"] mod helper;'

run_file_case "E0c: include! of another file" red "E0c:" events \
  '/^use crate::track_vcs;$/a\
include!("../elsewhere.rs");'

# E1: the inline module set. A fourth inline module is a descendant of `events` and can call the private appender.
run_file_case "E1: a fourth inline module appears" red "E1:" events \
  '/^use crate::track_vcs;$/a\
mod smuggled { }'

# The ratchet has to bite in both directions.
run_file_case "E1: an inline module is renamed away" red "E1:" events \
  's/^mod gated {$/mod gated_renamed {/'

# E2: the exported entry set

run_file_case "E2: a third exported entrance" red "E2:" events \
  '/^use crate::track_vcs;$/a\
pub async fn append_without_a_gate(tx: \&mut Transaction<'"'"'_, Sqlite>) -> Result<i64> { unimplemented!() }'

# Also trips E3 (its pattern is `pub`-anchored); pins E2, the rule it is for.
run_file_case "E2: an entrance is demoted to private" red "E2:" events \
  's/^pub async fn append_decision_event_in_tx($/async fn append_decision_event_in_tx(/'

# E3: neither entrance regrows a policy parameter
run_file_case "E3: single entrance regrows a gate parameter" red \
  "E3: \`append_decision_event_in_tx\`'s signature changed" events \
  's/^    event: &Event,$/    event: \&Event,\
    gate: \&PermissiveGate,/'

run_file_case "E3: batch entrance regrows a gate parameter" red \
  "E3: \`append_decision_events_in_tx\`'s signature changed" events \
  's/^    events: &\[Event\],$/    events: \&[Event],\
    gate: \&PermissiveGate,/'

# E4: the capability type keeps its shape. `0,/…/s//…/` replaces only the FIRST match: the struct field and `authorize`'s parameter are the same text, and the struct comes first.
run_file_case "E4: a capability field becomes pub" red \
  "E4: \`Authorized\`'s field block changed" events \
  '0,/^        actor: &'"'"'a ActorId,$/s//        pub actor: \&'"'"'a ActorId,/'

# The retargeting bypass, restored through the front door.
run_file_case "E4: a setter appears on the capability" red \
  "E4: \`Authorized\`'s inherent impl changed" events \
  '/^        pub(in crate::db::sqlite::events) fn event(&self) -> &'"'"'a Event {$/i\
        pub(in crate::db::sqlite::events) fn set_event(\&mut self, e: \&'"'"'a Event) { self.event = e; }'

# E5: mint and append name the same transaction binding. `Authorized` binds the triple, not the transaction, so the property is pinned textually.
run_file_case "E5: a mint moves to a different transaction binding" red "E5:" events \
  's/^    let authorized = gated::authorize(tx, actor, scope, event).await?;$/    let authorized = gated::authorize(gate_tx, actor, scope, event).await?;/'

# The shrink direction: an append site disappears from the census.
run_file_case "E5: an append site disappears" red "E5:" events \
  '/SqlxRepo::event_append_in_tx(tx, actor, scope, event, None).await/d'

# D1: the test-only gate abstraction keeps its cfg. Each subject gets a decoy line inserted directly above the item rather than a deleted cfg: deleting hits all four at once, and the detached cfg is what `attrs_above`'s adjacency exists to catch.
run_file_case "D1: cfg detached from the trait" red "D1: \`trait DecisionGate\`" gate \
  '/^pub trait DecisionGate: Send + Sync {$/i\
const D1_DECOY: () = ();'

run_file_case "D1: cfg detached from PermissiveGate" red "D1: \`struct PermissiveGate\`" gate \
  '/^pub struct PermissiveGate;$/i\
const D1_DECOY: () = ();'

run_file_case "D1: cfg detached from the impl" red \
  "D1: \`impl DecisionGate for PermissiveGate\`" gate \
  '/^impl DecisionGate for PermissiveGate {$/i\
const D1_DECOY: () = ();'

run_file_case "D1: cfg detached from commit_decision" red "D1: \`fn commit_decision\`" gate \
  '/^pub async fn commit_decision<R, G, F>($/i\
const D1_DECOY: () = ();'

# A subject that vanishes must be red too, or renaming it turns its rule into a no-op.
run_file_case "D1: a subject is renamed away" red \
  "D1: no \`struct PermissiveGate\` declaration found" gate \
  's/^pub struct PermissiveGate;$/pub struct PermissiveStub;/'

# GREEN: a doc comment between the attribute block and its item is ordinary Rust; the stripper blanks it and `attrs_above` must not treat the blank as a reset.
run_file_case "green: doc comment between the cfg and PermissiveGate" green "" gate \
  '/^pub struct PermissiveGate;$/i\
/// rationale line that must not break adjacency'

# S1: the events-insert census. Its subject is the whole repository, so the fixture is a purpose-built temporary git tree (a real `git init`, since the gate enumerates with `git ls-files`) plus a matching baseline, both passed by environment variable.
run_scan_case() {
  local name="$1" expect="$2" want_msg="$3" extra_file="$4" extra_body="$5" a_body="$6"
  cases=$((cases + 1))
  local dir; dir="$(mktemp -d)"
  mkdir -p "$dir/sub"
  printf '%s\n' "$a_body" > "$dir/a.rs"
  printf 'fn b() {\n  q("INSERT INTO events (kind) VALUES (1)");\n  q("insert  into events (kind) VALUES (2)");\n}\n' > "$dir/sub/b.rs"
  if [ -n "$extra_file" ]; then
    printf '%s\n' "$extra_body" > "$dir/$extra_file"
  fi
  git -C "$dir" init -q
  git -C "$dir" add -A

  local output rc
  set +e
  output="$(cd "$repo_root" && APPEND_SEAM_SCAN_ROOT="$dir" \
    APPEND_SEAM_INSERT_BASELINE="a.rs:1
sub/b.rs:2" "$gate" 2>&1)"
  rc=$?
  set -e
  check "$name" "$expect" "$want_msg" "$output" "$rc"
  rm -rf "$dir"
}

scan_a_with_insert='fn a() { q("INSERT INTO events (kind) VALUES (1)"); }'
scan_a_without_insert='fn a() { q("SELECT 1"); }'

# `sub/b.rs` pins a count of 2 only because the lower-case, double-spaced `insert  into events` is counted too; counts are MATCHING LINES, so two inserts on one line pin as 1.
run_scan_case "green: the pinned census matches the tree" green "" "" "" "$scan_a_with_insert"

run_scan_case "S1: a new file writes the events table" red "S1:" \
  "c.rs" 'fn c() { q("INSERT INTO events (kind) VALUES (3)"); }' "$scan_a_with_insert"

# Both directions: a baseline that only grows is one nobody has to update.
run_scan_case "S1: a pinned occurrence disappears" red "S1:" \
  "" "" "$scan_a_without_insert"

echo "----"
echo "$cases case(s), $failures failure(s)"
[ "$failures" -eq 0 ] || exit 1
