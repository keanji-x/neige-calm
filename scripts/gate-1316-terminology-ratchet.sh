#!/usr/bin/env bash
# Retiring-vocabulary ratchet: occurrence counts per (term, scope) against a
# committed baseline tsv. Both directions fail: a rise is new retiring vocabulary,
# a fall means tighten the ratchet (`--update-baseline`, commit the tsv).
# A baseline is only valid for the tree it was generated on — regenerate after every merge of upstream.
# Patterns are boundary-anchored per case; the two words' uppercase branches are deliberately
# asymmetric (`COVE[A-Z]+` is English, `WAVE[A-Z]+` is oracle ids) and `.spec.ts` is excluded by lookahead.

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
count() { # $1=pattern $2=scope
  local matches grep_status

  matches="$(git grep -P -o -h "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>/dev/null)"
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

ensure_baseline_inputs_are_tracked() {
  local untracked

  if ! untracked="$(git ls-files --others --exclude-standard -- "${RATCHETED_SCOPES[@]}")"; then
    echo "::error::could not check ratcheted scopes for untracked files; refusing to update $BASELINE." >&2
    return 1
  fi

  if [ -n "$untracked" ]; then
    echo "::error::refusing to update $BASELINE: git grep would omit these untracked files from ratcheted scopes:" >&2
    while IFS= read -r path; do
      printf '  %s\n' "$path" >&2
    done <<<"$untracked"
    echo "::error::Stage intended files with git add (or git add -N), then rerun --update-baseline." >&2
    return 1
  fi
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
  ensure_baseline_inputs_are_tracked || exit 1

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
  # `git grep` only reads TRACKED paths, so `git add -N` puts the probe under the same scan a real commit would get.
  fail_git_dir=''
  bad_tsv="$(mktemp)" || exit 1
  trap 'git rm -q --cached --force -- "$probe" >/dev/null 2>&1; rm -f "$probe" "$bad_tsv"; if [ -n "$fail_git_dir" ]; then rm -f "$fail_git_dir/git"; rmdir "$fail_git_dir"; fi' EXIT
  fails=0

  # A tsv copy with a non-integer count must be red BY THE ROW VALIDATOR (judged with `case`, not grep).
  while IFS= read -r line; do case "$line" in cove*) line="${line}x" ;; esac; printf '%s\n' "$line"; done <"$BASELINE" >"$bad_tsv"
  bad_tsv_output="$("./$SELF" --baseline "$bad_tsv" 2>&1)" && { echo "SELFTEST FAIL: a tsv copy with a non-integer count was accepted as green"; fails=1; }
  case "$bad_tsv_output" in
    *"::error::$bad_tsv:"*'is not a non-negative integer'*) echo "selftest ok: a tsv copy with a non-integer count is red by the row validator, naming the row" ;;
    *) echo "SELFTEST FAIL: a tsv copy with a non-integer count is not red by the row validator:"$'\n'"$bad_tsv_output"; fails=1 ;;
  esac

  # Baseline generation must fail closed before opening the baseline for write: `git grep` silently omits untracked files.
  baseline_hash="$(git hash-object -- "$BASELINE")"
  printf 'let x = wave_id;\n' >"$probe"
  if update_output="$("./$SELF" --update-baseline 2>&1)"; then
    echo "SELFTEST FAIL: --update-baseline accepted an untracked file in a ratcheted scope"
    fails=1
  elif ! grep -Fq "$probe" <<<"$update_output"; then
    echo "SELFTEST FAIL: --update-baseline rejected an untracked file without naming it"
    fails=1
  elif [ "$(git hash-object -- "$BASELINE")" != "$baseline_hash" ]; then
    echo "SELFTEST FAIL: rejected --update-baseline changed the existing baseline"
    fails=1
  else
    echo "selftest ok: --update-baseline rejects an untracked input and preserves the baseline"
  fi

  printf 'let x = cove_id;\n' >"$probe"
  git add -N -- "$probe" || exit 1

  # The wrapper fails only `git grep` and delegates every other git command to the real binary.
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
