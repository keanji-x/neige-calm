#!/usr/bin/env bash

set -euo pipefail

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"
fixtures="$repo_root/tests/fixtures/ci-ratchets"
# shellcheck source=lib.sh
. "$script_dir/lib.sh"

ratchets=(
  "$script_dir/runtimes_retirement.sh"
  "$script_dir/dropped_runtimes_table.sh"
)
clean_fixture_dir=clean/crates
mutation_case_paths=(
  runtimes-retirement/bare-table-dml
  runtimes-retirement/parity-scaffolding
  runtimes-retirement/retired-write-helper
  dropped-runtimes-table/ddl-reintroduction
  dropped-runtimes-table/quoted-dml
)
mutation_case_errors=(
  "runtimes table SQL"
  worker_sessions_parity
  runtime_start_tx
  "dropped 'runtimes' table reintroduced via DDL/JOIN/REPLACE"
  "dropped 'runtimes' table reintroduced via quoted-identifier SQL"
)
infrastructure_case_paths=(
  infrastructure/tool-missing
  infrastructure/path-missing
)
registered_fixture_dirs=(
  "$clean_fixture_dir"
  "${mutation_case_paths[@]}"
  "${infrastructure_case_paths[@]}"
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

run_fixture_registry_case() {
  local disk_dirs
  local missing_dirs
  local registered_dirs
  local unregistered_dirs

  if [ "${#mutation_case_paths[@]}" -ne "${#mutation_case_errors[@]}" ]; then
    echo "selftest failure: mutation fixture paths and expected errors are not one-to-one" >&2
    exit 1
  fi
  disk_dirs="$(find "$fixtures" -mindepth 2 -maxdepth 2 -type d \
    | sed "s#^$fixtures/##" \
    | LC_ALL=C sort)"
  registered_dirs="$(printf '%s\n' "${registered_fixture_dirs[@]}" | LC_ALL=C sort)"
  missing_dirs="$(comm -23 \
    <(printf '%s\n' "$registered_dirs") \
    <(printf '%s\n' "$disk_dirs"))"
  unregistered_dirs="$(comm -13 \
    <(printf '%s\n' "$registered_dirs") \
    <(printf '%s\n' "$disk_dirs"))"

  if [ -n "$missing_dirs" ] || [ -n "$unregistered_dirs" ]; then
    [ -z "$missing_dirs" ] || printf 'selftest failure: registered fixture missing on disk: %s\n' "$missing_dirs" >&2
    [ -z "$unregistered_dirs" ] || printf 'selftest failure: fixture directory is not registered: %s\n' "$unregistered_dirs" >&2
    exit 1
  fi
  echo "PASS fixture registry: disk and registered fixture directories match"
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

run_infrastructure_case() {
  local case_path="$1"
  local output
  local rc
  local missing_root="$fixtures/infrastructure/path-missing/does-not-exist"

  case "$case_path" in
    infrastructure/tool-missing)
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
      ;;
    infrastructure/path-missing)
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
      ;;
    *)
      echo "selftest failure: unknown infrastructure fixture: $case_path" >&2
      exit 1
      ;;
  esac
}

run_scan_command_error_case() {
  local output
  local rc

  require_tool rg
  set +e
  output="$(scan_must_be_empty \
    "invalid rg option probe" \
    rg --type=nosuchtype x "$fixtures/clean" 2>&1)"
  rc=$?
  set -e
  count_errors "$output"
  if [ "$rc" -eq 0 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: scan-command-error did not fail closed" >&2
    exit 1
  fi
  case "$error_lines" in
    *"scan infrastructure failed with exit"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: scan-command-error did not identify the infrastructure exit" >&2
      exit 1
      ;;
  esac
  echo "PASS infrastructure scan-command-error: nonzero exit; error named scan infrastructure failure"
}

run_fixture_registry_case
run_clean_case
for i in "${!mutation_case_paths[@]}"; do
  case_path="${mutation_case_paths[$i]}"
  run_single_mutation_case \
    "${case_path##*/}" \
    "$fixtures/$case_path" \
    "${mutation_case_errors[$i]}"
done
for case_path in "${infrastructure_case_paths[@]}"; do
  run_infrastructure_case "$case_path"
done
run_scan_command_error_case

echo "OK: runtimes ratchet selftest passed"
