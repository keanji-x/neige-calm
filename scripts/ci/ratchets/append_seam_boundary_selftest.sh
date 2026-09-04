#!/usr/bin/env bash

set -euo pipefail

# #1252 S3′ PR-B — fixtures for `append_seam_boundary.sh`.
#
# This repository has shipped ratchets that were green for months because the
# tool they invoked was not installed, and ratchets whose rule had been deleted
# while their fixture kept passing. So: **every rule in the gate has a fixture
# here that has been watched go red, and each fixture pins a substring of the
# message of the rule it is for.** Exit-1-for-any-reason is not evidence — the
# gate also exits 1 when a subject file is missing.
#
# Two disciplines, both inherited from `report_write_boundary_selftest.sh`:
#
#   * **One violation per red case.** A fixture whose failure could come from
#     either of two rules cannot tell you which one is live. Three cases here
#     legitimately trip two rules and say so in their own comment; each still
#     pins the message of the rule it is *for*, and every failure is printed, so
#     a pinned substring can only have come from the intended rule.
#
#     The no-op check below proves a case's mutation changed *something*. It
#     does not prove the change was one line, landed where intended, or is legal
#     Rust.
#
#   * **The assertion path spawns no process.** The substring test is bash's own
#     `case`, not `printf | grep -qF`: the pipeline version failed roughly once
#     in forty runs in the report-write suite, reporting "red, but not for the
#     stated reason" while printing output that plainly contained the substring.
#     A false RED destroys a gate faster than a false GREEN, because people
#     learn to re-run it until it passes.
#
# The green fixtures are the real production files, not hand-written
# miniatures: a miniature drifts and the suite keeps passing while the gate
# stops matching the file it actually runs on. The one exception is S1, whose
# subject is the whole repository — that rule gets a purpose-built temporary git
# tree, and the case list says why below.

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

# run_file_case <name> <green|red> <msg> <events|gate> <sed-program>
#
# Copies the named production file, applies the sed program to the copy, points
# the gate at the copy, and checks the exit class (plus the message, for red
# cases). The other subject file, and the repository census, stay real — so a
# red case here can only be red because of the mutated file.
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

  # Applies to GREEN cases too: a green case whose sed silently stops matching
  # degrades into "run the gate on the production file", which the first case
  # already does. It would keep passing while testing nothing.
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

# --- the production tree, unmodified, must pass ------------------------------
#
# Without this, every red case below could be passing because the gate is broken
# in general.
run_file_case "green: production events.rs as-is" green "" events ""

# --- E0a / E0b / E0c: readability and the module escape hatches --------------

run_file_case "E0a: block comment" red "E0a:" events \
  '/^use crate::track_vcs;$/a\
/* nothing to see here */'

# A raw IDENTIFIER. Raw strings must stay legal — `events.rs` is full of
# `r#"INSERT INTO events …"#` — so this rule is narrower than the report-write
# gate's blanket `r#` ban, and the green case above is what proves the SQL still
# passes.
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

# --- E1: the inline module set ----------------------------------------------
#
# A fourth inline module is a descendant of `events`, so it can name
# `gated::Authorized` and call the private appender. That is a legitimate thing
# to do and an illegitimate thing to do quietly.
run_file_case "E1: a fourth inline module appears" red "E1:" events \
  '/^use crate::track_vcs;$/a\
mod smuggled { }'

# The ratchet has to bite in both directions: a one-way ratchet has no pawl.
# Renaming `gated` away means the pinned set no longer describes the file.
run_file_case "E1: an inline module is renamed away" red "E1:" events \
  's/^mod gated {$/mod gated_renamed {/'

# --- E2: the exported entry set ---------------------------------------------

run_file_case "E2: a third exported entrance" red "E2:" events \
  '/^use crate::track_vcs;$/a\
pub async fn append_without_a_gate(tx: \&mut Transaction<'"'"'_, Sqlite>) -> Result<i64> { unimplemented!() }'

# Also trips E3 (the pattern that finds the signature is `pub`-anchored, so the
# flattened signature comes back empty). Pins E2, which is the rule it is for.
run_file_case "E2: an entrance is demoted to private" red "E2:" events \
  's/^pub async fn append_decision_event_in_tx($/async fn append_decision_event_in_tx(/'

# --- E3: neither entrance regrows a policy parameter -------------------------
#
# This is the exact shape #1252 S3′ deleted: an injected `DecisionGate`, whose
# only production implementor was the allow-everything `PermissiveGate`.
run_file_case "E3: single entrance regrows a gate parameter" red \
  "E3: \`append_decision_event_in_tx\`'s signature changed" events \
  's/^    event: &Event,$/    event: \&Event,\
    gate: \&PermissiveGate,/'

run_file_case "E3: batch entrance regrows a gate parameter" red \
  "E3: \`append_decision_events_in_tx\`'s signature changed" events \
  's/^    events: &\[Event\],$/    events: \&[Event],\
    gate: \&PermissiveGate,/'

# --- E4: the capability type keeps its shape ---------------------------------
#
# `0,/…/s//…/` replaces only the FIRST match: the struct field and `authorize`'s
# parameter are the same eight-space text, and the struct comes first.
run_file_case "E4: a capability field becomes pub" red \
  "E4: \`Authorized\`'s field block changed" events \
  '0,/^        actor: &'"'"'a ActorId,$/s//        pub actor: \&'"'"'a ActorId,/'

# The retargeting bypass, restored through the front door. This is the sample
# `append_seam_escape_probe::retarget` exists to keep failing (E0616).
run_file_case "E4: a setter appears on the capability" red \
  "E4: \`Authorized\`'s inherent impl changed" events \
  '/^        pub(in crate::db::sqlite::events) fn event(&self) -> &'"'"'a Event {$/i\
        pub(in crate::db::sqlite::events) fn set_event(\&mut self, e: \&'"'"'a Event) { self.event = e; }'

# --- E5: mint and append name the same transaction binding -------------------
#
# `Authorized` binds the triple, not the transaction, and no clean type fixes
# that: making it borrow the transaction makes the append line E0499 (verified
# with a minimal repro). So the property is pinned textually and this fixture is
# what shows the pin is live.
run_file_case "E5: a mint moves to a different transaction binding" red "E5:" events \
  's/^    let authorized = gated::authorize(tx, actor, scope, event).await?;$/    let authorized = gated::authorize(gate_tx, actor, scope, event).await?;/'

# The shrink direction: an append site disappears from the census.
run_file_case "E5: an append site disappears" red "E5:" events \
  '/SqlxRepo::event_append_in_tx(tx, actor, scope, event, None).await/d'

# --- D1: the test-only gate abstraction keeps its cfg ------------------------
#
# Each of the four subjects gets the *decoy* construction rather than a deleted
# cfg: deleting the attribute line hits all four subjects at once (it is the
# same text four times), and a four-violation fixture cannot show which rule is
# live. Inserting one non-attribute line directly above the item leaves the cfg
# in the file, four lines up, attached to nothing — which is precisely what
# `attrs_above`'s adjacency exists to catch, and what a `grep -B N` window would
# miss.
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

# A subject that vanishes must be red too — otherwise renaming `PermissiveGate`
# turns its rule into a no-op that reports success.
run_file_case "D1: a subject is renamed away" red \
  "D1: no \`struct PermissiveGate\` declaration found" gate \
  's/^pub struct PermissiveGate;$/pub struct PermissiveStub;/'

# GREEN — the false-red regression. A doc comment between the attribute block
# and its item is ordinary Rust; the stripper blanks that line and `attrs_above`
# must not treat the blank as a reset. Paired with the four red cases above,
# this distinguishes "the rule is live" from "the rule fires at anything".
run_file_case "green: doc comment between the cfg and PermissiveGate" green "" gate \
  '/^pub struct PermissiveGate;$/i\
/// rationale line that must not break adjacency'

# --- S1: the events-insert census -------------------------------------------
#
# S1's subject is the whole repository, so its fixtures cannot be a copy of one
# production file. They are a purpose-built temporary git tree plus a matching
# baseline, both passed in by environment variable. It is a real `git init`
# tree, not a bare directory, because the gate enumerates with `git ls-files`
# (the repository keeps sibling worktrees under `.claude/worktrees/`, which a
# `find` would walk into) — a `find`-based fixture would exercise a code path
# CI never runs.
#
# The gate's other two subjects stay real in these cases, so the only rule that
# can speak is S1.
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

# The green fixture for S1. It also proves the two tolerances the pattern was
# given are real: `sub/b.rs` pins a count of 2 only because the lower-case,
# double-spaced `insert  into events` on its own line is counted as well. The
# counts are MATCHING LINES, not occurrences — `grep -c` counts lines, so two
# inserts on one line pin as 1.
run_scan_case "green: the pinned census matches the tree" green "" "" "" "$scan_a_with_insert"

run_scan_case "S1: a new file writes the events table" red "S1:" \
  "c.rs" 'fn c() { q("INSERT INTO events (kind) VALUES (3)"); }' "$scan_a_with_insert"

# Both directions. A baseline that only grows is a baseline nobody has to
# update, and a rule nobody has to update is a rule nobody reads.
run_scan_case "S1: a pinned occurrence disappears" red "S1:" \
  "" "" "$scan_a_without_insert"

echo "----"
echo "$cases case(s), $failures failure(s)"
[ "$failures" -eq 0 ] || exit 1
