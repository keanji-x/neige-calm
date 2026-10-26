#!/usr/bin/env bash

set -euo pipefail

# Fixtures for `report_write_boundary.sh`: mutate copies of the production file and assert the rule diagnostic.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

gate="$script_dir/report_write_boundary.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

real_boundary="$repo_root/crates/calm-server/src/track_report/write.rs"
[ -f "$real_boundary" ] || {
  echo "::error::production boundary file missing: $real_boundary"
  exit 1
}

failures=0
cases=0

# run_case <name> <green|red> <expected-substring-when-red> <sed-program>: applies the sed program to a copy of the real boundary file and points the gate at it.
run_case() {
  local name="$1" expect="$2" want_msg="$3" program="$4"
  cases=$((cases + 1))
  local dir; dir="$(mktemp -d)"
  local file="$dir/write.rs"
  sed "$program" "$real_boundary" > "$file"

  # Applies to GREEN cases too: a green case whose sed silently stops matching degrades into the first case and tests nothing.
  if [ -n "$program" ] && cmp -s "$file" "$real_boundary"; then
    echo "FAIL [$name]: the mutation changed nothing — the sed program no longer matches the production file, so this case is testing the green fixture"
    failures=$((failures + 1))
    rm -rf "$dir"
    return
  fi

  local output rc
  set +e
  output="$(cd "$repo_root" && REPORT_WRITE_BOUNDARY_FILE="$file" "$gate" 2>&1)"
  rc=$?
  set -e

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
  rm -rf "$dir"
}

# Production stays green; each mutation must change the file and identify its rule.
run_case "green: production boundary as-is" green "" ""

run_case "R1: writer becomes pub(crate)" red "R1: the writer is declared" \
  's/async fn persist(/pub(crate) async fn persist(/'

run_case "R1: writer renamed" red "R1: no top-level" \
  's/fn persist(/fn persist_inner(/'

# G1: an attribute before mod on the same line must not hide the declaration.
run_case "R2: attributed submodule off column zero" red "R2:" \
  '1i\
#[cfg(any())] mod helper;'

run_case "R2: #[path] module" red "R2:" \
  '1i\
#[r#path = "../elsewhere.rs"]'

run_case "R2: include! of another file" red "R2:" \
  '1i\
r#include!("../elsewhere.rs");'

echo "----"
echo "$cases case(s), $failures failure(s)"
[ "$failures" -eq 0 ] || exit 1
