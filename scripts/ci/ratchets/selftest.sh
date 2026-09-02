#!/usr/bin/env bash

set -euo pipefail

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"
fixtures="$repo_root/tests/fixtures/ci-ratchets"
ratchets=(
  "$script_dir/runtimes_retirement.sh"
  "$script_dir/dropped_runtimes_table.sh"
)

error_count=0
error_lines=
count_errors() {
  local line

  error_count=0
  error_lines=
  while IFS= read -r line; do
    case "$line" in
      ::error::*)
        error_count=$((error_count + 1))
        error_lines+="$line"$'\n'
        ;;
    esac
  done <<< "$1"
}

run_clean_case() {
  local script
  local output
  local rc

  for script in "${ratchets[@]}"; do
    set +e
    output="$("$script" "$fixtures/clean" 2>&1)"
    rc=$?
    set -e
    if [ "$rc" -ne 0 ]; then
      printf '%s\n' "$output"
      echo "selftest failure: clean fixture failed under ${script##*/}" >&2
      exit 1
    fi
    count_errors "$output"
    if [ "$error_count" -ne 0 ]; then
      printf '%s\n' "$output"
      echo "selftest failure: clean fixture emitted ::error:: under ${script##*/}" >&2
      exit 1
    fi
  done
  echo "PASS clean: both ratchets accepted legal 0055 regression fixture"
}

run_single_mutation_case() {
  local name="$1"
  local fixture="$2"
  local expected_error_fragment="$3"
  local script
  local output
  local rc
  local failed_cases=0
  local total_errors=0
  local all_error_lines=

  for script in "${ratchets[@]}"; do
    set +e
    output="$("$script" "$fixture" 2>&1)"
    rc=$?
    set -e
    if [ "$rc" -ne 0 ]; then
      failed_cases=$((failed_cases + 1))
    fi
    count_errors "$output"
    total_errors=$((total_errors + error_count))
    all_error_lines+="$error_lines"
  done

  if [ "$failed_cases" -ne 1 ] || [ "$total_errors" -ne 1 ]; then
    printf '%s' "$all_error_lines"
    echo "selftest failure: $name made $failed_cases ratchet cases fail with $total_errors errors; expected exactly one" >&2
    exit 1
  fi
  case "$all_error_lines" in
    *"$expected_error_fragment"*) ;;
    *)
      printf '%s' "$all_error_lines"
      echo "selftest failure: $name error did not name '$expected_error_fragment'" >&2
      exit 1
      ;;
  esac
  echo "PASS mutation $name: exactly one case failed; error named $expected_error_fragment"
}

run_infrastructure_cases() {
  local output
  local rc
  local missing_root="$fixtures/infrastructure/path-missing/does-not-exist"

  set +e
  output="$(PATH="$fixtures/infrastructure/tool-missing/empty-bin" /bin/bash "${ratchets[0]}" "$fixtures/clean" 2>&1)"
  rc=$?
  set -e
  count_errors "$output"
  if [ "$rc" -ne 1 ] || [ "$error_count" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: missing-tool fixture did not fail closed exactly once" >&2
    exit 1
  fi
  case "$error_lines" in
    *"required tool 'rg'"*) ;;
    *)
      printf '%s' "$error_lines"
      echo "selftest failure: missing-tool error did not name rg" >&2
      exit 1
      ;;
  esac
  echo "PASS infrastructure tool-missing: exit 1; error named rg"

  set +e
  output="$("${ratchets[0]}" "$missing_root" 2>&1)"
  rc=$?
  set -e
  count_errors "$output"
  if [ "$rc" -ne 1 ] || [ "$error_count" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: missing-path fixture did not fail closed exactly once" >&2
    exit 1
  fi
  case "$error_lines" in
    *"required scan path does not exist"*) ;;
    *)
      printf '%s' "$error_lines"
      echo "selftest failure: missing-path error did not identify the absent path" >&2
      exit 1
      ;;
  esac
  echo "PASS infrastructure path-missing: exit 1; error identified absent scan path"
}

run_clean_case
run_single_mutation_case \
  bare-table-dml \
  "$fixtures/runtimes-retirement/bare-table-dml" \
  "runtimes table SQL"
run_single_mutation_case \
  parity-scaffolding \
  "$fixtures/runtimes-retirement/parity-scaffolding" \
  "worker_sessions_parity"
run_single_mutation_case \
  retired-write-helper \
  "$fixtures/runtimes-retirement/retired-write-helper" \
  "runtime_start_tx"
run_single_mutation_case \
  ddl-reintroduction \
  "$fixtures/dropped-runtimes-table/ddl-reintroduction" \
  "dropped 'runtimes' table reintroduced via DDL/JOIN/REPLACE"
run_single_mutation_case \
  quoted-dml \
  "$fixtures/dropped-runtimes-table/quoted-dml" \
  "dropped 'runtimes' table reintroduced via quoted-identifier SQL"
run_infrastructure_cases

echo "OK: runtimes ratchet selftest passed"
