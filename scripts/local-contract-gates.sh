#!/usr/bin/env bash
# Run the workspace's package-wide contract tests locally: goldens, registries and
# covers-every-X checks, byte budgets and source scans. A targeted `-p <pkg> <filter>`
# run does not select them, so without this script CI is the first to catch them.
# The set is fixed, not derived from the diff. Name patterns select most of it, so a new
# `*golden*` or `*invariant*` test in one of these binaries joins on its own. Every selected test is pure: no
# process spawn except `git ls-files`, no long timers.
# Usage: scripts/local-contract-gates.sh [--list]
#   --list prints the selected tests instead of running them.

set -uo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "error: not inside a git working tree; run from a neige-calm checkout" >&2
  exit 2
}
cd "$root" || exit 2

mode=run
case "${1:-}" in
  '') ;;
  --list) mode=list ;;
  *)
    echo "usage: scripts/local-contract-gates.sh [--list]" >&2
    exit 2
    ;;
esac

# Same build conventions as the other local gates. CARGO_TARGET_DIR passes through.
export RUSTC_WRAPPER=""
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

# One filterset for every group. Name patterns first; module prefixes only where a
# contract suite's test names share no pattern.
filters=(
  'test(/golden|invariant|covers_(every|exactly|all)_|cover_exactly_|fits_its_byte_budget/)'
  'package(plugin-runtime) & test(/^(manifest|template_input|config|perms|glob)::/)'
  'test(/every_root_test_file_is_in_this_suite/)'
  'test(/^(mcp_server::tools::tests|codex_appserver::tool_names_kernel_tests)::/)'
  'test(/^mcp_server::wiring::tests::terminal_policy_/)'
  'test(/^mcp_server::cli::commands::tests::(every_|help_documents_|prompt_|task_report_surfaces_)/)'
  'test(/^templates::tests::builtin_directory_and_roster_are_the_same_set$/)'
  'test(/^routes::codex::tests::every_codex_worker_hook_is_registered/)'
  'test(/^(no_retired_tool_names|handle_state_writers|planner_attachments_guarded_surface|openapi|track_write_point_registry|head_schema_fixture)::/)'
  'test(/^events_pruner::no_other_suite_seeds_/)'
  'test(/^bounded_track_tree_sql::every_recursive_parent_track_cte_/)'
  'test(/^no_wildcard_wait_in_the_supervisor_host$/)'
)
filterset="$(printf ' | %s' "${filters[@]}")"
filterset="${filterset:3}"

# Build only the binaries that hold the selected tests. `--lib`/`--test` apply to every
# listed package, so packages that need different targets are separate groups.
groups=(
  'plugin runtime:-p plugin-runtime --lib'
  'calm-server+calm-types:-p calm-server -p calm-types --lib --test replay_event_suite --test kernel_process_suite --test mcp_core_suite --test planner_harness_suite --test migration_suite --test runtime_dispatch_suite --test worker_flow_claude_suite --test worker_flow_codex_suite --test domain_api_suite'
  'integration suites:-p calm-truth -p calm-exec -p calm-codex-bridge -p calm-session -p calm-proc-supervisor --test integration_suite --test no_wildcard_wait_in_the_supervisor_host'
)

declare -A result=()
names=()
for group in "${groups[@]}"; do
  name="${group%%:*}"
  read -r -a targets <<<"${group#*:}"
  names+=("$name")
  printf '\n=== %s\n' "$name"
  if [ "$mode" = list ]; then
    cmd=(cargo nextest list --locked "${targets[@]}" -E "$filterset")
  else
    cmd=(cargo nextest run --locked "${targets[@]}" -E "$filterset" --no-fail-fast --test-threads 8)
  fi
  if env -u NEIGE_CODEX_BIN "${cmd[@]}"; then result[$name]=PASS; else result[$name]=FAIL; fi
done

printf '\n=== summary\n'
failed=0
for name in "${names[@]}"; do
  printf '%s  %s\n' "${result[$name]}" "$name"
  [ "${result[$name]}" = PASS ] || failed=1
done
exit "$failed"
