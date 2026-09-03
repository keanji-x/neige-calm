#!/usr/bin/env bash
# #1316 S0 — retiring-vocabulary ratchet.
#
# WHY THIS SHAPE AND NOT #1209's
#
# `scripts/gate-1209-template-rename-residual.sh` is the end state this repo
# already knows how to run: a whole-repo scan with a per-file allowlist where
# every entry states why it is there and pins how many lines it may match. That
# shape works because `workflow` was down to ~30 files when the gate landed.
#
# #1316 starts from ~10.9k matching lines for `cove` and ~26.8k for `wave`
# across ~900 files. A per-file allowlist of that size would be unreadable and
# nobody would ever check an entry's reason, which is precisely the fig leaf the
# #1209 header warns about. So S0 ships the OTHER standard shape — a ratchet —
# and S6 converts it to the #1209 allowlist form once the counts are small.
#
# THE RULE, BOTH DIRECTIONS
#
#   actual > baseline  => FAIL. New retiring vocabulary entered the tree.
#   actual < baseline  => FAIL. The ratchet must be tightened; run
#                         `--update-baseline` and commit the new numbers.
#
# The second direction is not pedantry. A baseline left at an old, higher number
# silently re-permits every line the slice just removed. A ratchet that only
# checks one direction is a ratchet with no pawl.
#
# WHAT EACH PATTERN COVERS, STATED HONESTLY
#
# `cove` / `wave`
#   Both are this project's coinages. A scan of every `[a-z]*wave[a-z]*` form in
#   `crates fe docs e2e` returns no ordinary-English use — no `wavelength`, no
#   `microwave`, no `waveform`. The bare case-insensitive substring is therefore
#   exact, not a heuristic, and needs no allowlist.
#
# `spec` — the PLANNER-AGENT sense (#1316 B class)
#   Narrowed three ways and STILL over-broad, deliberately:
#     * `(?<![a-z])spec(?![a-z])` excludes `specific`, `specifier`,
#       `specification`, `inspect`.
#     * `spec_x` / `x_spec` catch the identifier forms (`spec_card_id`,
#       `spec_thread_id`, `spec_task_ceiling`, `event_spec`).
#     * `.spec.ts` / `.spec.tsx` are EXCLUDED by lookahead. That is the
#       Vitest/Playwright filename convention on 735 lines; it is not ours and
#       renaming it is not in scope for any slice.
#   What remains over-broad: the bare word in prose about an OpenAPI spec or a
#   cookie spec. No regex separates "the spec agent" from "the OpenAPI spec", so
#   the ratchet counts both. Consequence to accept knowingly: adding the phrase
#   "OpenAPI spec" to a comment in `crates/` raises that cell and fails this
#   gate. The honest fix at that moment is to write "OpenAPI schema" (which is
#   what the repo calls it everywhere else) — not to widen the pattern.
#
# `runtime_id` — the RETIRED-ID sense ONLY (#1316 B class, narrowed at S0)
#   `runtime` matches 7307 lines in scope; only 1496 are this. The umbrella
#   issue originally said "Runtime 退役", which was too wide for its carrier and
#   is now narrowed: the verified claim is `crates/calm-types/src/runtime.rs:18`
#   `pub type RuntimeId = String;` immediately above
#   `WorkerSessionProjection { id: RuntimeId }` — the runtime id IS the worker
#   session id, two spellings in one file. Everything else spelled `Runtime` is
#   a DIFFERENT concept and is not scanned: `PluginRuntimeStatus` (plugin
#   process state), `OperationRuntime` (`operation/driver.rs`), `ProcRuntime`,
#   `runtime_layer`, and ordinary English "fails at runtime".
#
# `harness_item` — the TRANSCRIPT sense ONLY (#1316 B class, narrowed at S0)
#   Same correction. `crates/calm-server/src/harness/` is `registry.rs`,
#   `run_loop.rs`, `lock.rs`, `state.rs`, `snapshot.rs` — a RUNNING agent
#   instance that claims a registry slot (`spawn_recovered_harness`,
#   `try_reserve`), not a log. Renaming those to `Transcript*` would be a worse
#   name than the one it replaces. Only `harness_items` / `HarnessItem` is
#   genuinely a protocol transcript, so only that is scanned.
#   Left open on purpose: the harness registry is keyed by `runtime_id`, i.e.
#   `runtime` / `harness` / `worker_session` are three names for layers of one
#   execution concept. That is a design question, not a rename, and #1316
#   explicitly refuses to smuggle it into a mechanical slice.
#
# SCOPES
#   Ratcheted: `crates` `fe` `docs` `e2e`.
#   Informational only: `web` — owner decision, the legacy bundle is being
#   deleted, so it gets minimal compile-only fixes and must not gate anything.
#   It is still REPORTED so its deletion shows up as a number.
#
# PROVE IT DISCRIMINATES
#   `--selftest` runs the positive/negative pair required of any gate here:
#     * inject `cove_id` into a file in a ratcheted scope  => must FAIL
#     * inject `specification` and "fails at runtime"      => must PASS
#     * inject `foo.spec.ts`                               => must PASS
#   It uses a scratch file it creates and deletes; it never mutates tracked
#   files.

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1

SELF='scripts/gate-1316-terminology-ratchet.sh'
BASELINE='scripts/gate-1316-terminology-ratchet.baseline.tsv'

# term<TAB>pattern. Reasons for each are in the header above.
read -r -d '' TERMS <<'EOF' || true
cove	(?i)cove
wave	(?i)wave
spec	(?i)(?<![a-z])spec(?![a-z.])|(?i)spec(?!\.tsx?)_[a-z]|(?i)[a-z]_spec(?![a-z])|AiSpec|SpecHarness|SpecAgent|SPEC_
runtime_id	(?i)runtime[_-]?id|RuntimeId
harness_item	(?i)harness_item|HarnessItem
EOF

RATCHETED_SCOPES=(crates fe docs e2e)
INFO_SCOPES=(web)

count() { # $1=pattern $2=scope
  git grep -P -c "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>/dev/null |
    awk -F: '{s+=$NF} END {print s+0}'
}

emit_baseline() {
  echo "# #1316 retiring-vocabulary ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are MATCHING LINES per (term, scope). Only-down is enforced."
  printf '# term\tscope\tcount\n'
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    for scope in "${RATCHETED_SCOPES[@]}"; do
      printf '%s\t%s\t%s\n' "$term" "$scope" "$(count "$pattern" "$scope")"
    done
  done <<<"$TERMS"
}

if [ "${1:-}" = '--update-baseline' ]; then
  emit_baseline >"$BASELINE"
  echo "wrote $BASELINE"
  exit 0
fi

if [ "${1:-}" = '--selftest' ]; then
  probe='crates/calm-types/src/_gate_1316_selftest_probe.rs'
  # `git grep` only reads TRACKED paths, which is the right behaviour in CI
  # (everything under review is committed) but makes a plain untracked probe
  # invisible — the first version of this selftest reported a pass for an
  # injection the gate had never actually seen. `git add -N` puts the probe in
  # the index so the probe is subject to the same scan a real commit would be.
  trap 'git rm -q --cached --force -- "$probe" >/dev/null 2>&1; rm -f "$probe"' EXIT
  fails=0

  printf 'let x = cove_id;\n' >"$probe"
  git add -N -- "$probe" || exit 1
  if "./$SELF" >/dev/null 2>&1; then
    echo "SELFTEST FAIL: injected 'cove_id' did not trip the gate"; fails=1
  else
    echo "selftest ok: injected 'cove_id' trips the gate"
  fi

  printf 'The specification is loaded; a bad config fails at runtime.\nSee foo.spec.ts for the case.\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "selftest ok: 'specification' / 'at runtime' / 'foo.spec.ts' do not trip it"
  else
    echo "SELFTEST FAIL: ordinary English tripped the gate"; "./$SELF"; fails=1
  fi

  exit "$fails"
fi

if [ ! -f "$BASELINE" ]; then
  echo "::error::$BASELINE is missing. Generate it with: ./$SELF --update-baseline"
  exit 1
fi

declare -A EXPECTED=()
while IFS=$'\t' read -r term scope want; do
  case "$term" in ''|'#'*) continue ;; esac
  EXPECTED["$term/$scope"]="$want"
done <"$BASELINE"

fail=0
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${RATCHETED_SCOPES[@]}"; do
    key="$term/$scope"
    got="$(count "$pattern" "$scope")"
    want="${EXPECTED[$key]:-}"
    if [ -z "$want" ]; then
      echo "::error::$BASELINE has no row for '$key'. Run --update-baseline."
      fail=1
    elif [ "$got" -gt "$want" ]; then
      echo "::error::$key rose from $want to $got matching lines — new '$term' vocabulary entered the tree (#1316 is retiring it)."
      fail=1
    elif [ "$got" -lt "$want" ]; then
      echo "::error::$key fell from $want to $got. Tighten the ratchet: ./$SELF --update-baseline, then commit $BASELINE. A baseline left high re-permits every line this change removed."
      fail=1
    fi
  done
done <<<"$TERMS"

echo "--- informational only (not gated; the legacy bundle is being deleted) ---"
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${INFO_SCOPES[@]}"; do
    printf '    %-14s %-6s %s\n' "$term" "$scope" "$(count "$pattern" "$scope")"
  done
done <<<"$TERMS"

if [ "$fail" -ne 0 ]; then
  echo "::error::#1316 S0 terminology ratchet failed. This is drift control over a stated baseline, not a proof that any slice is complete; see the header of $SELF."
  exit 1
fi

echo "OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope."
