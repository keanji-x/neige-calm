#!/usr/bin/env bash
# Sourced baseline mechanics. Entrypoints own terms, scopes, headers, diagnostics
# for rises and update failures, and their independent selftest probes.

ratchet_count() { # pattern, pathspec, optional captured-error detail
  local matches grep_status errfile=/dev/null
  if [ "${3:-}" = detail ]; then errfile="$(mktemp)" || return 1; fi
  matches="$(git grep --untracked -P -o -h "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>"$errfile")"
  grep_status=$?
  case "$grep_status" in
    0) printf '%s\n' "$matches" | wc -l; grep_status=$?
       if [ "${3:-}" = detail ]; then grep_status=0; fi ;;
    1) printf '0\n'; grep_status=0 ;;
    *)
      local detail=''
      if [ "${3:-}" = detail ]; then detail=" git said: $(<"$errfile")"; fi
      echo "::error::git grep failed with exit $grep_status while scanning '$2'; refusing to use a partial count.$detail" >&2
      ;;
  esac
  if [ "${3:-}" = detail ]; then
    rm -f -- "$errfile"
  fi
  return "$grep_status"
}

declare -A EXPECTED=()
load_baseline() { # $1=tsv path
  local line term scope want rest tabs n=0
  EXPECTED=()
  [ -f "$1" ] || { echo "::error::$1 is missing. Generate it with: ./$SELF --update-baseline"; return 1; }
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    case "$line" in *$'\r'*) echo "::error::$1:$n contains a carriage return; the baseline must be LF-only."; return 1 ;; esac
    case "$line" in ''|'#'*) continue ;; esac
    tabs="${line//[!$'\t']/}"
    [ "${#tabs}" -eq 2 ] || { echo "::error::$1:$n has ${#tabs} tab(s), expected exactly 2 (term<TAB>scope<TAB>count): '$line'"; return 1; }
    term="${line%%$'\t'*}"; rest="${line#*$'\t'}"; scope="${rest%%$'\t'*}"; want="${rest#*$'\t'}"
    { [ -n "$term" ] && [ -n "$scope" ]; } || { echo "::error::$1:$n has an empty term or scope: '$line'"; return 1; }
    case "$want" in ''|*[!0-9]*) echo "::error::$1:$n count '$want' is not a non-negative integer: '$line'"; return 1 ;; esac
    [ "${#want}" -le 12 ] || { echo "::error::$1:$n count '$want' has more than 12 digits; bash compares signed 64-bit integers and a longer count would error into a green result: '$line'"; return 1; }
    [ -z "${EXPECTED[$term/$scope]+x}" ] || { echo "::error::$1:$n duplicates the row for '$term/$scope'."; return 1; }
    EXPECTED["$term/$scope"]="$want"
  done <"$1"
}


ratchet_emit_rows() {
  local term pattern scope found i
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    for i in "${!RATCHETED_SCOPES[@]}"; do
      scope="${RATCHETED_SCOPES[$i]}"
      found="$(count "$pattern" "${RATCHETED_PATHS[$i]}")" || return 1
      printf '%s\t%s\t%s\n' "$term" "$scope" "$found"
    done
  done <<<"$TERMS"
}

ratchet_update_baseline() { # optional mktemp error, replacement error
  local baseline_tmp
  if ! baseline_tmp="$(mktemp "$BASELINE.tmp.XXXXXX")"; then
    [ -z "$1" ] || echo "::error::$1" >&2
    return 1
  fi
  trap 'rm -f -- "$baseline_tmp"' EXIT
  trap 'exit 130' HUP INT TERM
  if ! emit_baseline >"$baseline_tmp"; then
    echo "::error::failed to generate $BASELINE; the existing baseline was preserved." >&2
    exit 1
  fi
  if ! chmod 0644 "$baseline_tmp" || ! mv -- "$baseline_tmp" "$BASELINE"; then
    echo "::error::$2" >&2
    exit 1
  fi
  trap - EXIT HUP INT TERM
  echo "wrote $BASELINE"
}

ratchet_compare() {
  local term pattern scope key got want i
  fail=0
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    for i in "${!RATCHETED_SCOPES[@]}"; do
      scope="${RATCHETED_SCOPES[$i]}"
      key="$term/$scope"
      got="$(count "$pattern" "${RATCHETED_PATHS[$i]}")" || { fail=1; continue; }
      case "$got" in ''|*[!0-9]*) echo "::error::the count for '$key' is not an integer ('$got'); refusing to compare."; fail=1; continue ;; esac
      [ "${#got}" -le 12 ] || { echo "::error::the count for '$key' has more than 12 digits ('$got'); refusing to compare."; fail=1; continue; }
      want="${EXPECTED[$key]:-}"
      if [ -z "$want" ]; then
        echo "::error::$BASELINE_FILE has no row for '$key'. Run --update-baseline."; fail=1
      elif [ "$got" -gt "$want" ]; then
        ratchet_rose_error; fail=1
      elif [ "$got" -lt "$want" ]; then
        echo "::error::$key fell from $want to $got. Tighten the ratchet: ./$SELF --update-baseline, then commit $BASELINE. A baseline left high re-permits every occurrence this change removed."; fail=1
      fi
    done
  done <<<"$TERMS"
}
