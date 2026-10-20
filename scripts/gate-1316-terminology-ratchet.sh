#!/usr/bin/env bash
# Retiring-vocabulary ratchet: occurrence counts per (term, scope) against a
# committed baseline tsv. Both directions fail: a rise is new retiring vocabulary,
# a fall means tighten the ratchet (`--update-baseline`, commit the tsv).
# A baseline is only valid for the tree it was generated on — regenerate after every merge of upstream.
# r16 upstream composition: the same title references add 11 crates occurrences
# and one FE pagination import to main's 298/135 baseline, yielding 309/136.
# Current #2858/#2859 composition: main's 300 crates occurrences plus the same
# 11 title references yields 311; the FE composition remains 136.
# #2774 bounded exception: harness_item/crates 334 -> 348 (on main after #2843) covers the frozen
# `harness_items` table in the new conversation-floor migration and its bounded transcript page
# and history reads, the current-session fence on its existing writers, the `HarnessItem` row type
# of the new repository read, and fixtures that now seed retired history as stored rows; no new
# domain name. The final occurrence seeds a historical terminal-outcome read fixture after
# the current-session fence began refusing that fixture's non-current writer; no scan rule changes.
# #2774 FE: harness_item/fe 141 -> 142 is the one current-transcript query key a new Planner
# conversation refreshes before it opens; the earlier conversation's read reuses the existing page.
# Latest-only conversation refresh: harness_item/fe 136 -> 140 is the owner
# row type and page-limit imports, a real page-options caller and replacement test key.
# The switch test reuses its fixed row fixture; no scan rule changes.
# Upstream receipt/version references retain their own bounded exception below.
# Restored-input transfer test: harness_item/crates 298 -> 299 is one SQL trigger
# on the existing transcript table; production DELETE is factored without a new reference.
# Conversation-title projection bounded exception: harness_item/crates 288 -> 298 covers
# two storage-owner SELECTs of the frozen transcript table plus eight test references to
# that table and its existing insert/delete APIs. harness_item/fe 106 -> 107 is the frozen
# pagination-limit import, aliased PAGE_LIMIT locally. No new domain name or scan rule;
# ordinary new conversations and tests need no production inventory entry.
# #2618 review-fix exception: harness_item/crates 282 -> 290 covers eight test-only SQL
# references to the frozen transcript table: rollback timestamps and persisted rewind contexts.
# No new domain identifier or scan rule; ordinary tests need no new inventory entry.
# #2618 bounded exception: harness_item/crates 278 -> 282 covers three frozen transcript-table
# references in the storage owner's SELECTs and one compatibility test updating a real producer
# row into the released provider-owned shape; no new domain identifier. Future SQL touching
# that physical table updates this baseline and rationale.
# #2343 bounded exception: harness_item/crates 274 -> 278 covers two test-only deletes plus
# the storage owner's receipt projection read/rebuild of the frozen table; no new domain name.
# #1913 bounded exception: spec/fe 59 -> 63 comes solely from the pinned
# temporal-spec transitive dependency in fe/package-lock.json (dependency key,
# package path and two archive URL tokens). No retired application concept is added.
# #1923 bounded exception: harness_item/crates 262 -> 264 comes solely from the two SQL statements
# of the rewind that read and delete rows of the `harness_items` table, whose name stays; the new
# Rust names say `transcript`.
# #2003 bounded exception: harness_item/crates 264 -> 266 comes solely from the one-time tool-name
# migration's UPDATE of the `harness_items` table and its test's read-back of one row; no new name.
# Development publication rename: harness_item/crates 266 -> 268 comes solely from
# migration 0137 and its verification query against the existing transcript table.
# #2087 B0 bounded exception: harness_item/crates 268 -> 269 comes solely from migration 0141's
# one UPDATE of the existing transcript table; its test reads rows back through a shared helper.
# #2087 B1 bounded exception: harness_item/crates 269 -> 270 comes solely from migration 0142's
# one UPDATE of the existing transcript table; its test reads rows back through the same helper.
# #2087 B1b bounded exception: harness_item/crates 270 -> 271 comes solely from migration 0143's
# one UPDATE of the existing transcript table; its test reads rows back through the same helper.
# #2087 B1c bounded exception: harness_item/crates 271 -> 272 comes solely from migration 0144's
# one UPDATE of the existing transcript table; its test reads rows back through the same helper.
# #2130 bounded exception: harness_item/crates 272 -> 273 comes solely from the one SQL statement
# that reads a Planner session's newest turn-input row (`transcript_latest_turn_input_tx`); the new
# Rust names say `transcript` and `turn_input`.
# #2087 B5 bounded exception: harness_item/crates 273 -> 274 comes solely from migration 0148's
# one UPDATE of the existing transcript table; its test reads rows back through the same helper.
# #2184 bounded exception: spec/crates 496 -> 501 comes solely from five uses of the frozen
# `planner-harness-start` payload key `spec_card_id` (the fresh-start predicate's SQL, the
# operation rows of its unit test and of the in-flight route test, and two tests counting a
# card's start operations); harness_item/crates
# 274 -> 275 is the predicate's one read of the existing transcript table plus two test inserts,
# net of two `reset_harness_items` field references removed with today_summary's retry.
# #2227 bounded exception: harness_item/crates 275 -> 276 comes solely from the native plugin
# tool-name migration's one UPDATE of the existing transcript table; its test reads rows back
# through the same helper.
# #2235 bounded exception: harness_item/fe 95 -> 97 comes solely from imports of the
# existing wire row type and pagination limit in the production-route benchmark;
# local names use TranscriptRow and PAGE_LIMIT. No new protocol vocabulary is defined.
# #2665 bounded exception: harness_item/fe 97 -> 99 comes solely from the cached-switch
# regression's existing wire row import (aliased TranscriptRow) and existing query-key
# producer. No new domain identifier, schema, or scan rule is introduced.
# #2768 bounded exception: harness_item/fe 103 -> 106 comes solely from the
# integrated send-visibility regression's existing wire type import/use and existing
# query-key producer. Production names and protocol contracts are unchanged.
# #2718 bounded exception: harness_item/fe 99 -> 103 comes solely from the existing
# wire row type import and the canonical derivation export's import, mock key and
# actual access in a transparent production-function spy. Local names use TranscriptRow
# and toTranscriptTurns; no new domain identifier, schema, or scan rule is introduced.
# #2129 upgrade compatibility: one existing event_spec protocol field reference in the
# frozen pre-2129 descriptor, kept as replay test data; no new domain term.
# #2617 bounded exception: spec/crates 500 -> 501 comes solely from the CI evidence lowerer's
# import of the existing `event_spec` payload builder, aliased `event_contract` as the issue
# lowerer does; no new domain term.
# #2617 bounded exception: spec/crates 501 -> 503 comes solely from two struct literals of the
# existing `event_spec` payload field (the event-free rerun payload and the approval gate's test
# payload); no new domain term.
# #2671 bounded compatibility references: spec/crates 503 -> 508 adds five uses of
# the existing serialized HarnessProfile::Planner value `spec`: the conversation
# schema CHECK, three SQL fixtures, and the serialization compatibility test.
# Moving the existing serde rename from server to calm-types is net zero.
# #2671 bounded transcript references: harness_item/crates 288 -> 298 adds six
# references to the existing physical harness_items table (one FK, two storage
# reads and three adapter test SQL statements), plus four calls to the existing
# authoritative harness_item_insert API in real relationship fixtures. No new
# protocol name, wrapper, scan rule or growing file/test inventory is introduced.
# Patterns are boundary-anchored per case; the two words' uppercase branches are deliberately
# asymmetric (`COVE[A-Z]+` is English, `WAVE[A-Z]+` is oracle ids) and `.spec.ts` is excluded by lookahead.

# #2740 bounded contract exception: runtime_id/crates 1224 -> 1243 and fe 0 -> 32
# cover the declared receipt/current-presence wire fields, typed constructors and consumers,
# serde/schema oracle, generated declarations, actual-response fixtures, and existing TxOutput key reads.
# harness_item/crates 288 -> 298 covers the existing transcript repository read and tests
# of projection failure, restart transfer, version floors and provider-bound acceptance on the frozen table.
# harness_item/fe 106 -> 135 covers existing wire-row/derivation imports, fixtures and query keys
# used to pin actual identity/version, acknowledgements and page handoff. No new domain name,
# excluded file, pattern, count rule or executable policy is introduced; ordinary additional
# contract references update this counted baseline without adding a file/test roster.

# #2671 final compatibility delta: spec/crates 508 -> 509 is one negative
# decoding fixture using the released start key spec_card_id. harness_item/crates
# 298 -> 326 counts reads, foreign keys and fault assertions on the frozen
# transcript table, existing row/repository types and APIs, and the historical
# route/query contracts reused by the negotiated reader. harness_item/fe 135 ->
# 145 counts those existing wire/schema/query APIs and generated historical-row
# alternatives. New transport DTOs use Conversation names rather than adding
# retired domain names. runtime_id/fe 32 -> 35 is one original receipt fixture
# plus two generated copies of the existing receipt field's documentation.
# runtime_id/crates 1243 -> 1129 tightens the removed duplicate orchestration.
# These are bounded contract references, not excluded paths or a growing roster.
# Ordinary later edits still use this same occurrence scan; no new check runs.
# #2671 delayed-history browser verification: harness_item/fe 145 -> 146 is one
# use of the existing authoritative transcript query key to prove that the real
# cache still holds the earlier page while its response is delayed. No protocol,
# domain identifier, assertion, scan pattern or excluded path changes.

# PR #2843 catch-up composes the already-counted #2671 references with main's
# independent title references: crates 323 + 311 - 300 = 334, FE 144 + 136 - 135 = 145.
# No additional exception, scan exclusion, pattern or growing inventory is added.

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1

SELF='scripts/gate-1316-terminology-ratchet.sh'
BASELINE='scripts/gate-1316-terminology-ratchet.baseline.tsv'
# `--baseline <path>` compares against another tsv (the selftest's malformed copy).
BASELINE_FILE="$BASELINE"
if [ "${1:-}" = '--baseline' ]; then BASELINE_FILE="${2:?--baseline needs a path}"; shift 2; fi

# term<TAB>pattern.
read -r -d '' TERMS <<'EOF' || true
cove	(?<![a-zA-Z])coves?(?![a-z])|Coves?(?![a-z])|COVES?(?![A-Z])
wave	(?<![a-zA-Z])waves?(?![a-z])|Waves?(?![a-z])|WAVE
spec	(?i)(?<![a-z])spec(?![a-z.])|(?i)spec(?!\.tsx?)_[a-z]|(?i)[a-z]_spec(?![a-z])|AiSpec|SpecHarness|SpecAgent|SPEC_
runtime_id	(?i)runtime[_-]?id|RuntimeId
harness_item	(?i)harness_item|HarnessItem
EOF

RATCHETED_SCOPES=(crates fe docs e2e)
INFO_SCOPES=(web)

# Counts OCCURRENCES, not matching lines: `-o` emits one line per match, so appending a second match to an already-matching line still moves the count.
# `--untracked` scans tracked and untracked files in the working tree and skips ignored ones.
count() { # $1=pattern $2=scope
  local matches grep_status

  matches="$(git grep --untracked -P -o -h "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>/dev/null)"
  grep_status=$?
  case "$grep_status" in
    0) printf '%s\n' "$matches" | wc -l ;;
    1) printf '0\n' ;;
    *)
      echo "::error::git grep failed with exit $grep_status while scanning '$2'; refusing to use a partial count." >&2
      return "$grep_status"
      ;;
  esac
}

emit_baseline() {
  local found

  echo "# #1316 retiring-vocabulary ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are OCCURRENCES per (term, scope). Only-down is enforced."
  printf '# term\tscope\tcount\n'
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    for scope in "${RATCHETED_SCOPES[@]}"; do
      if ! found="$(count "$pattern" "$scope")"; then
        return 1
      fi
      printf '%s\t%s\t%s\n' "$term" "$scope" "$found"
    done
  done <<<"$TERMS"
}

if [ "${1:-}" = '--update-baseline' ]; then
  if ! baseline_tmp="$(mktemp "$BASELINE.tmp.XXXXXX")"; then
    echo "::error::could not create a temporary baseline next to $BASELINE." >&2
    exit 1
  fi
  cleanup_baseline_tmp() { rm -f -- "$baseline_tmp"; }
  trap cleanup_baseline_tmp EXIT
  trap 'exit 130' HUP INT TERM

  if ! emit_baseline >"$baseline_tmp"; then
    echo "::error::failed to generate $BASELINE; the existing baseline was preserved." >&2
    exit 1
  fi
  if ! chmod 0644 "$baseline_tmp" || ! mv -- "$baseline_tmp" "$BASELINE"; then
    echo "::error::could not replace $BASELINE; the existing baseline was preserved." >&2
    exit 1
  fi
  trap - EXIT HUP INT TERM
  echo "wrote $BASELINE"
  exit 0
fi

if [ "${1:-}" = '--selftest' ]; then
  probe='crates/calm-types/src/_gate_1316_selftest_probe.rs'
  # `*.swp` is in .gitignore, so this path is ignored in every ratcheted scope.
  ignored_probe="$probe.swp"
  fail_git_dir=''
  bad_tsv="$(mktemp)" || exit 1
  trap 'rm -f "$probe" "$ignored_probe" "$bad_tsv"; if [ -n "$fail_git_dir" ]; then rm -f "$fail_git_dir/git"; rmdir "$fail_git_dir"; fi' EXIT
  fails=0

  # A tsv copy with a non-integer count must be red BY THE ROW VALIDATOR (judged with `case`, not grep).
  while IFS= read -r line; do case "$line" in cove*) line="${line}x" ;; esac; printf '%s\n' "$line"; done <"$BASELINE" >"$bad_tsv"
  bad_tsv_output="$("./$SELF" --baseline "$bad_tsv" 2>&1)" && { echo "SELFTEST FAIL: a tsv copy with a non-integer count was accepted as green"; fails=1; }
  case "$bad_tsv_output" in
    *"::error::$bad_tsv:"*'is not a non-negative integer'*) echo "selftest ok: a tsv copy with a non-integer count is red by the row validator, naming the row" ;;
    *) echo "SELFTEST FAIL: a tsv copy with a non-integer count is not red by the row validator:"$'\n'"$bad_tsv_output"; fails=1 ;;
  esac

  # An ignored file never reaches a commit, so the scan skips it.
  printf 'let x = cove_id;\n' >"$ignored_probe"
  if ! git check-ignore -q -- "$ignored_probe"; then
    echo "SELFTEST FAIL: $ignored_probe is not ignored; choose an ignored probe path"
    fails=1
  elif ignored_output="$("./$SELF" 2>&1)"; then
    echo "selftest ok: 'cove_id' in an ignored file is not counted"
  else
    echo "SELFTEST FAIL: 'cove_id' in an ignored file was counted:"$'\n'"$ignored_output"
    fails=1
  fi
  rm -f -- "$ignored_probe"

  # A new file that was never `git add`ed is part of the change, so the scan counts it.
  printf 'let x = cove_id;\n' >"$probe"
  untracked_output="$("./$SELF" 2>&1)" && { echo "SELFTEST FAIL: 'cove_id' in an untracked file did not trip the gate"; fails=1; }
  if [ -n "$(git ls-files -- "$probe")" ]; then
    echo "SELFTEST FAIL: $probe is tracked; the untracked case is not exercised"
    fails=1
  else
    case "$untracked_output" in
      *'cove/crates rose from '*) echo "selftest ok: 'cove_id' in an untracked file trips the gate on cove/crates" ;;
      *) echo "SELFTEST FAIL: 'cove_id' in an untracked file did not raise cove/crates:"$'\n'"$untracked_output"; fails=1 ;;
    esac
  fi

  # The wrapper fails only `git grep` and delegates every other git command to the real binary.
  baseline_hash="$(git hash-object -- "$BASELINE")"
  fail_git_dir="$(mktemp -d)" || exit 1
  cat >"$fail_git_dir/git" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = grep ]; then
  exit 128
fi
exec "$GATE_1316_REAL_GIT" "$@"
EOF
  chmod +x "$fail_git_dir/git" || exit 1
  if scan_error_output="$(PATH="$fail_git_dir:$PATH" GATE_1316_REAL_GIT="$(command -v git)" "./$SELF" --update-baseline 2>&1)"; then
    echo "SELFTEST FAIL: --update-baseline accepted a git grep failure"
    fails=1
  elif ! grep -Fq 'git grep failed with exit 128' <<<"$scan_error_output"; then
    echo "SELFTEST FAIL: --update-baseline did not report the git grep failure"
    fails=1
  elif [ "$(git hash-object -- "$BASELINE")" != "$baseline_hash" ]; then
    echo "SELFTEST FAIL: failed baseline scan changed the existing baseline"
    fails=1
  else
    echo "selftest ok: --update-baseline propagates scan errors and preserves the baseline"
  fi

  printf 'The specification is loaded; a bad config fails at runtime.\nSee foo.spec.ts for the case.\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "selftest ok: 'specification' / 'at runtime' / 'foo.spec.ts' do not trip it"
  else
    echo "SELFTEST FAIL: ordinary English tripped the gate"; "./$SELF"; fails=1
  fi

  printf 'Recovery is recoverable: the reaper recovered every uncovered branch it discovers.\nCoverage covers what the audit covered; the reviewer waved it through while wavering.\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "selftest ok: recover/discover/cover/coverage/waved/wavering are not counted"
  else
    echo "SELFTEST FAIL: ordinary English containing 'cove'/'wave' as a substring tripped the gate"
    "./$SELF"; fails=1
  fi

  # A following UPPERCASE letter is ours (camelCase) and `WAVE[A-Z]+` oracle ids must stay counted — the anchoring must not be too tight.
  printf 'const x = coveConversations; const y = onWaveRoute;\n// E-CAP-WAVECREATE INV-WAVEROW COVES\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "SELFTEST FAIL: camelCase / oracle-id forms of our own vocabulary went uncounted — the anchoring is too tight"
    fails=1
  else
    echo "selftest ok: coveConversations / onWaveRoute / CAP-WAVEROW / COVES are still counted"
  fi

  # Both implementations are red here; only the delta tells occurrence-counting from line-counting.
  printf 'cove cove cove\n' >"$probe"
  delta="$("./$SELF" 2>&1 | sed -n 's/.*cove\/crates rose from \([0-9]*\) to \([0-9]*\).*/\2-\1/p' | head -1)"
  if [ -n "$delta" ] && [ "$((${delta}))" -eq 3 ]; then
    echo "selftest ok: 3 matches on 1 line move the count by 3 (occurrence-counted, not line-counted)"
  else
    echo "SELFTEST FAIL: 3 matches on 1 line moved the count by '${delta:-<no rose-from message>}', expected 3 — the ratchet is line-counting again and can be bypassed by appending to a matching line"
    fails=1
  fi

  exit "$fails"
fi

if [ ! -f "$BASELINE_FILE" ]; then
  echo "::error::$BASELINE_FILE is missing. Generate it with: ./$SELF --update-baseline"
  exit 1
fi

# Rows are validated first: `[ "$got" -gt "$want" ]` on `824x` is a bash error (status 2) that sets NEITHER branch = green.
declare -A EXPECTED=()
n=0
while IFS= read -r line || [ -n "$line" ]; do
  n=$((n + 1))
  case "$line" in *$'\r'*) echo "::error::$BASELINE_FILE:$n contains a carriage return; the baseline must be LF-only."; exit 1 ;; esac
  case "$line" in ''|'#'*) continue ;; esac
  tabs="${line//[!$'\t']/}"
  [ "${#tabs}" -eq 2 ] || { echo "::error::$BASELINE_FILE:$n has ${#tabs} tab(s), expected exactly 2 (term<TAB>scope<TAB>count): '$line'"; exit 1; }
  term="${line%%$'\t'*}"; rest="${line#*$'\t'}"; scope="${rest%%$'\t'*}"; want="${rest#*$'\t'}"
  { [ -n "$term" ] && [ -n "$scope" ]; } || { echo "::error::$BASELINE_FILE:$n has an empty term or scope: '$line'"; exit 1; }
  case "$want" in ''|*[!0-9]*) echo "::error::$BASELINE_FILE:$n count '$want' is not a non-negative integer: '$line'"; exit 1 ;; esac
  [ "${#want}" -le 12 ] || { echo "::error::$BASELINE_FILE:$n count '$want' has more than 12 digits; bash compares signed 64-bit integers and a longer count would error into a green result: '$line'"; exit 1; }
  [ -z "${EXPECTED[$term/$scope]+x}" ] || { echo "::error::$BASELINE_FILE:$n duplicates the row for '$term/$scope'."; exit 1; }
  EXPECTED["$term/$scope"]="$want"
done <"$BASELINE_FILE"

fail=0
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${RATCHETED_SCOPES[@]}"; do
    key="$term/$scope"
    if ! got="$(count "$pattern" "$scope")"; then
      fail=1
      continue
    fi
    case "$got" in ''|*[!0-9]*) echo "::error::the count for '$key' is not an integer ('$got'); refusing to compare."; fail=1; continue ;; esac
    [ "${#got}" -le 12 ] || { echo "::error::the count for '$key' has more than 12 digits ('$got'); refusing to compare."; fail=1; continue; }
    want="${EXPECTED[$key]:-}"
    if [ -z "$want" ]; then
      echo "::error::$BASELINE_FILE has no row for '$key'. Run --update-baseline."
      fail=1
    elif [ "$got" -gt "$want" ]; then
      echo "::error::$key rose from $want to $got occurrences — new '$term' vocabulary entered the tree (#1316 is retiring it)."
      fail=1
    elif [ "$got" -lt "$want" ]; then
      echo "::error::$key fell from $want to $got. Tighten the ratchet: ./$SELF --update-baseline, then commit $BASELINE. A baseline left high re-permits every occurrence this change removed."
      fail=1
    fi
  done
done <<<"$TERMS"

echo "--- informational only (not gated; the legacy bundle is being deleted) ---"
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${INFO_SCOPES[@]}"; do
    if ! got="$(count "$pattern" "$scope")"; then
      fail=1
      continue
    fi
    printf '    %-14s %-6s %s\n' "$term" "$scope" "$got"
  done
done <<<"$TERMS"

if [ "$fail" -ne 0 ]; then
  echo "::error::#1316 S0 terminology ratchet failed. This is drift control over a stated baseline, not a proof that any slice is complete; see the header of $SELF."
  exit 1
fi

echo "OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope."
