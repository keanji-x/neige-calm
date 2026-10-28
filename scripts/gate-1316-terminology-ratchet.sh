#!/usr/bin/env bash
# U12a-1 bounded historical-contract exception: harness_item/crates 301 -> 303
# is two SQL reads of the frozen harness_items table in calm-server/src/routes/
# canonical_flow_tests/system_assistant_tests/authority.rs, in
# historical_summary_key_requires_explicit_migration_without_erasing_original_work:
# 1. Read original row ids/params before the System namespace rejection.
# 2. Read each original row's params afterward to prove history is preserved.
# These exercise the real historical namespace boundary, not new domain names.
# Later SQL-contract changes edit that test and regenerate this baseline; ordinary
# test additions need no inventory edit. Patterns/scopes/exclusions are unchanged.
# The existing normal occurrence scan runs once in CI lint, plus its existing
# separate --selftest step; no new check or invocation is added.

# B1a raw fixtures: harness_item/crates 298 -> 301 counts raw_tests.rs's
# CREATE, INSERT and independent expected entry for the real harness_items table.
# Keeping the historical name literal preserves raw coverage; no scan rule changes.
# The existing occurrence scan runs once in CI lint; no extra check is added.
# Ordinary new tables need no fixed inventory; references use this same baseline.
# System Assistant namespace owner bounded exception: runtime_id/crates 1108 -> 1109
# is the existing required original-carrier field in a typed Human receipt fixture.
# The test calls the real Human acceptance/creation owners to prove mutual permanent
# namespace exclusion. No new domain name, fallback, scan rule or invocation is added;
# later changes to this frozen receipt contract update its fixture and baseline.
# #3176 bounded contract exception: spec/crates 521 -> 523 adds the existing
# event_spec helper for the remove receipt and its serialized-field assertion.
# No new domain name, scan rule, exemption or check is introduced; the existing
# occurrence scan runs once in CI lint and later wire references update its baseline.
# #2774 Planner switch history: runtime_id/crates 1133 -> 1135 adds two
# reads of the frozen initiating-worker column, before and after switching.
# These assertions preserve historical attribution; no domain name, exclusion,
# scan rule or inventory is added. Future frozen-column queries use this baseline.
# ACP END settlement bounded exception: harness_item/crates 294 -> 297 is
# three SQL references to the frozen transcript table: the storage owner's
# idempotent observation query and the regression's row join/failure trigger.
# No domain name, scan rule, excluded path or test inventory is introduced.
# U3 bounded receipt exception: runtime_id/crates 1136 -> 1145 adds nine
# references to the existing original-carrier receipt field/column and its HTTP assertions.
# CI handoff regression: runtime_id/fe 36 -> 37 is one required
# initialRuntimeId field in the canonical first-input receipt fixture.
# No new runtime naming contract, scan exemption or test/file roster is introduced.
# #3050 bounded exception: spec/crates 529 -> 531 adds one read of the existing
# event_spec field in Forge failure policy and one matching fixture payload key.
# No new domain vocabulary; fixture authors and routes use their existing constants.
# Retiring-vocabulary ratchet: occurrence counts per (term, scope) against a
# committed baseline tsv. Both directions fail: a rise is new retiring vocabulary,
# a fall means tighten the ratchet (`--update-baseline`, commit the tsv).
# A baseline is only valid for the tree it was generated on — regenerate after every merge of upstream.
# U1b bounded exception: runtime_id/crates 1134 -> 1136 is two history
# assertions reading the frozen initiating-worker column. harness_item/crates
# 303 -> 304 is the exact-content carrier CAS read of the existing transcript table.
# No new domain identifiers or scan rules; future storage queries keep this ratchet.
# 0190 Grok Turn provider bounded exception: spec/crates +2, runtime_id/crates +3 and
# harness_item/crates +2: migration 0190 re-creates 0184's frozen canonical tables verbatim
# (spec 1, runtime_id 2, harness_item 1), its upgrade test inserts one raw Turn row (spec 1,
# runtime_id 1), and the Grok decline test reads the transcript's terminal row (harness_item 1).
# No new domain identifier or scan rule.
# r16 upstream composition: the same title references add 11 crates occurrences
# and one FE pagination import to main's 298/135 baseline, yielding 309/136.
# Current #2858/#2859 composition: main's 300 crates occurrences plus the same
# 11 title references yields 311; the FE composition remains 136.
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
# #2666 C3.2a bounded exception: spec/crates 509 -> 510 is the existing event_spec
# field in the typed forge payload of the clean-fence regression; no new domain term.
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

# #2836 Claude canonical slice 2 bounded exception: crates 334 -> 336 adds two
# test-only references to the frozen transcript table: the actual outcome-write
# failure trigger and the no-historical-user-projection assertion. No domain name,
# scan rule, excluded path, test inventory or additional CI pass is introduced.
# Later SQL references use the existing occurrence baseline as before.

# #2836 bounded persisted-contract exception: runtime_id/crates 1122 -> 1125
# counts three references to the frozen origin_runtime_id column when migration
# 0180/0181 replace 0175's creation scope trigger for canonical V3/V4. Every other
# scope predicate is retained verbatim; there is no new domain identifier,
# excluded path, scan rule or inventory. A later trigger migration retains the
# physical column name and updates this same counted baseline.

# #2836 retry storage bounded exception: migration 0184 recreates the cyclic
# schema and its V5 creation fence, retaining physical contract identifiers:
# runtime_id/crates 1128 -> 1134 (six occurrences), spec/crates 510 -> 511 (the
# frozen profile value), harness_item/crates 336 -> 337 (the presentation FK).
# Only the new migration adds these occurrences. The existing scan/baseline
# runs once in each ratchet gate target; no exclusion or extra inventory/check
# is added. Later schema changes update this same occurrence baseline.

# Planner compact-kind bounded contract exception: spec/crates 511 -> 523 retains
# the serialized Planner declaration author in the migration default (1), MCP
# schema (1), prompt and generated prompt golden (2), authority-fence explanation
# (1), and validator/contract/authority/scheduler/recovery fixtures (7).
# spec/fe 63 -> 66 is the same wire author in the read schema, its parse test,
# and the browser regression that renders the declared compact task.
# wave/crates 706 -> 708 preserves the released spawn default in the rebuilt
# tasks table and in a recovery-allocation fixture. No domain name or scan rule
# changes. Ordinary later contract references update the owning schema/test and
# this existing occurrence baseline; no file roster or additional CI pass is added.

# Planner compact execution bounded contract exception: spec/crates 523 -> 528
# retains the serialized declaration author in the context-full prompt and its
# generated golden (2), two report-to-scheduler fixtures (2), and the Claude
# historical stack declaration (1). These are references to the frozen wire
# author, not a domain name. The existing occurrence scan still runs once per
# ratchet target. A later serialized-author fixture edits its owning test and
# this existing baseline; no new scan, exclusion, or file/test inventory is added.

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

# Shared mechanics; vocabulary, scope and error context remain in this entrypoint.
source scripts/text-ratchet-engine.sh
count() { ratchet_count "$@"; }
RATCHETED_PATHS=("${RATCHETED_SCOPES[@]}")

emit_baseline() {
  echo "# #1316 retiring-vocabulary ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are OCCURRENCES per (term, scope). Only-down is enforced."
  printf '# term\tscope\tcount\n'
  ratchet_emit_rows
}

if [ "${1:-}" = '--update-baseline' ]; then
  ratchet_update_baseline "could not create a temporary baseline next to $BASELINE." \
    "could not replace $BASELINE; the existing baseline was preserved."
  exit "$?"
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

load_baseline "$BASELINE_FILE" || exit 1

ratchet_rose_error() {
  echo "::error::$key rose from $want to $got occurrences — new '$term' vocabulary entered the tree (#1316 is retiring it)."
}
ratchet_compare

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
