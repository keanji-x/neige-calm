#!/usr/bin/env bash

set -euo pipefail

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
gate="$script_dir/frozen-vector-gate.sh"
temp_root="$(mktemp -d)"
trap 'rm -rf -- "$temp_root"' EXIT
zero_sha=0000000000000000000000000000000000000000

new_fixture_repo() {
  local repo="$1"

  git init -q -b main "$repo"
  git -C "$repo" config user.name 'frozen-vector selftest'
  git -C "$repo" config user.email 'frozen-vector-selftest@example.invalid'
  mkdir -p "$repo/crates/calm-server/tests/vectors"
  printf 'base\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" add crates/calm-server/tests/vectors/case.json
  git -C "$repo" commit -qm 'fixture base'
  git -C "$repo" update-ref refs/remotes/origin/main HEAD
  git -C "$repo" switch -qc feature
}

run_non_tip_violation_case() {
  local repo="$temp_root/non-tip-violation"
  local bad_sha
  local output
  local rc

  new_fixture_repo "$repo"
  printf 'changed without rationale\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam 'change frozen vector without rationale'
  bad_sha="$(git -C "$repo" rev-parse HEAD)"
  printf 'last commit does not touch vectors\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated tip commit'

  set +e
  output="$(BASE_SHA="$zero_sha" \
    DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: non-tip vector violation exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $bad_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: gate did not identify the non-tip violating commit $bad_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS branch-creation multi-commit: non-tip vector violation was rejected"
}

run_valid_multi_commit_case() {
  local repo="$temp_root/valid-multi-commit"

  new_fixture_repo "$repo"
  printf 'changed with rationale\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'change frozen vector\n\nFROZEN-VECTOR-CHANGE: fixture rationale'
  printf 'last commit does not touch vectors\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated tip commit'

  BASE_SHA="$zero_sha" \
    DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS branch-creation multi-commit: marked non-tip vector change was accepted"
}

run_unresolvable_base_case() {
  local repo="$temp_root/unresolvable-base"
  local output
  local rc

  new_fixture_repo "$repo"
  git -C "$repo" update-ref -d refs/remotes/origin/main

  set +e
  output="$(BASE_SHA="$zero_sha" \
    DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: unresolvable base exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"default-branch ref 'refs/remotes/origin/main' cannot be resolved"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: unresolvable-base error did not identify the missing default-branch ref" >&2
      exit 1
      ;;
  esac
  echo "PASS infrastructure: unresolvable fallback base failed closed"
}

run_default_branch_creation_case() {
  local repo="$temp_root/default-branch-creation"
  local output
  local rc

  new_fixture_repo "$repo"
  printf 'unmarked default-branch creation change\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam 'unmarked vector change on recreated default branch'
  printf 'unrelated tip\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated recreated-default tip'
  git -C "$repo" branch -f main HEAD
  git -C "$repo" switch -q main
  git -C "$repo" update-ref refs/remotes/origin/main HEAD

  set +e
  output="$(BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: default-branch creation exited $rc; expected fail-closed exit 1" >&2
    exit 1
  fi
  case "$output" in
    *"default-branch merge-base equals audit head"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: default-branch creation did not reject its empty fallback range" >&2
      exit 1
      ;;
  esac
  echo "PASS branch-creation default branch: empty fallback range failed closed"
}

run_invalid_nonzero_base_case() {
  local repo="$temp_root/invalid-nonzero-base"
  local invalid_sha=1111111111111111111111111111111111111111
  local output
  local rc

  new_fixture_repo "$repo"

  set +e
  output="$(BASE_SHA="$invalid_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: invalid nonzero base exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"cannot be resolved; refusing the branch-creation fallback for a nonzero base"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: invalid nonzero base was not distinguished from branch creation" >&2
      exit 1
      ;;
  esac
  echo "PASS force-push infrastructure: invalid nonzero base failed closed"
}

run_rewind_case() {
  local repo="$temp_root/rewind"
  local before_sha
  local after_sha
  local output
  local rc

  new_fixture_repo "$repo"
  after_sha="$(git -C "$repo" rev-parse HEAD)"
  printf 'commit that will be rewound\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'future commit before rewind'
  before_sha="$(git -C "$repo" rev-parse HEAD)"

  set +e
  output="$(BASE_SHA="$before_sha" HEAD_SHA="$after_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: rewind range exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"is not an ancestor of audit head"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: rewind range did not identify a non-fast-forward base" >&2
      exit 1
      ;;
  esac
  echo "PASS force-push rewind: non-fast-forward range failed closed"
}

run_merge_history_violation_case() {
  local repo="$temp_root/merge-history-violation"
  local bad_sha
  local output
  local rc

  new_fixture_repo "$repo"
  git -C "$repo" switch -qc side
  printf 'unmarked side-branch change\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam 'unmarked side-branch vector change'
  bad_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" switch -q feature
  printf 'first-parent change\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated first-parent change'
  git -C "$repo" merge -q --no-ff -s ours side -m 'merge side while retaining first-parent vectors'

  set +e
  output="$(BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: merge-history violation exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $bad_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: full commit-graph audit missed side-branch commit $bad_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge history: pruned side-branch vector violation was rejected"
}

run_valid_merge_history_case() {
  local repo="$temp_root/valid-merge-history"

  new_fixture_repo "$repo"
  git -C "$repo" switch -qc side
  printf 'marked side-branch change\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked side-branch vector change\n\nFROZEN-VECTOR-CHANGE: fixture rationale'
  printf 'unrelated side tip after vector rationale\n' > "$repo/SIDE-NOTES.md"
  git -C "$repo" add SIDE-NOTES.md
  git -C "$repo" commit -qm 'unrelated side tip after marked vector change'
  git -C "$repo" switch -q feature
  printf 'first-parent change\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated first-parent change'
  git -C "$repo" merge -q --no-ff side -m 'ordinary merge without duplicate vector rationale'

  BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS merge history: marked side change did not require a duplicate merge marker"
}

run_marked_discarded_side_case() {
  local repo="$temp_root/marked-discarded-side"

  new_fixture_repo "$repo"
  git -C "$repo" switch -qc side
  printf 'marked side-branch change\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked side-branch vector change\n\nFROZEN-VECTOR-CHANGE: fixture rationale'
  git -C "$repo" switch -q feature
  printf 'first-parent change\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated first-parent change'
  git -C "$repo" merge -q --no-ff -s ours side -m 'discard marked side vector state'

  BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS merge history: discarding a marked side change did not require a merge marker"
}

run_default_branch_state_merge_case() {
  local repo="$temp_root/default-branch-state-merge"
  local base_sha

  new_fixture_repo "$repo"
  printf 'feature-only change\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'unrelated feature change'
  git -C "$repo" switch -q main
  printf 'unmarked state already on default branch\n' \
    > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam 'default-branch vector change already accepted upstream'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" update-ref refs/remotes/origin/main HEAD
  git -C "$repo" switch -q feature
  git -C "$repo" merge -q --no-ff main -m 'merge current main into feature'
  git -C "$repo" switch -q main
  git -C "$repo" merge -q --no-ff feature -m 'synthetic pull-request merge'

  BASE_SHA="$base_sha" DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS merge provenance: trusted default-branch state did not require a duplicate marker"
}

run_unmarked_branch_merge_case() {
  local repo="$temp_root/unmarked-branch-merge"
  local base_sha
  local merge_rc
  local merge_sha
  local output
  local rc

  new_fixture_repo "$repo"
  printf 'marked feature state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked feature vector state\n\nFROZEN-VECTOR-CHANGE: fixture feature rationale'
  git -C "$repo" switch -q main
  printf 'unmarked state already on default branch\n' \
    > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam 'default-branch vector change already accepted upstream'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" update-ref refs/remotes/origin/main HEAD
  git -C "$repo" switch -q feature

  set +e
  git -C "$repo" merge -q --no-ff main -m 'start conflicting upstream merge' >/dev/null 2>&1
  merge_rc=$?
  set -e
  if [ "$merge_rc" -eq 0 ]; then
    echo "selftest failure: unmarked branch-merge fixture did not produce the intended conflict" >&2
    exit 1
  fi
  printf 'third unmarked branch-merge state\n' \
    > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" add crates/calm-server/tests/vectors/case.json
  git -C "$repo" commit -qm 'unmarked branch merge resolution'
  merge_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" switch -q main
  git -C "$repo" merge -q --no-ff feature -m 'synthetic pull-request merge'

  set +e
  output="$(BASE_SHA="$base_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: unmarked branch merge exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $merge_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: trusted base laundered unmarked branch merge $merge_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge provenance: trusted base could not launder an unmarked third-state merge"
}

run_merge_resolution_violation_case() {
  local repo="$temp_root/merge-resolution-violation"
  local merge_rc
  local merge_sha
  local merge_parents
  local output
  local rc

  new_fixture_repo "$repo"
  git -C "$repo" switch -qc side
  printf 'marked side state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked side vector state\n\nFROZEN-VECTOR-CHANGE: fixture side rationale'
  git -C "$repo" switch -q feature
  printf 'marked first-parent state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked first-parent vector state\n\nFROZEN-VECTOR-CHANGE: fixture first-parent rationale'

  set +e
  git -C "$repo" merge -q --no-ff side -m 'start conflicting vector merge' >/dev/null 2>&1
  merge_rc=$?
  set -e
  if [ "$merge_rc" -eq 0 ]; then
    echo "selftest failure: merge-resolution fixture did not produce the intended conflict" >&2
    exit 1
  fi
  git -C "$repo" rev-parse -q --verify MERGE_HEAD >/dev/null \
    || { echo "selftest failure: merge-resolution fixture failed before creating MERGE_HEAD" >&2; exit 1; }
  printf 'third unmarked merge-resolution state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" add crates/calm-server/tests/vectors/case.json
  git -C "$repo" commit -qm 'unmarked vector merge resolution'
  merge_sha="$(git -C "$repo" rev-parse HEAD)"
  merge_parents="$(git -C "$repo" log -1 --format=%P "$merge_sha")"
  case "$merge_parents" in
    *' '*) ;;
    *)
      echo "selftest failure: merge-resolution fixture produced a one-parent commit" >&2
      exit 1
      ;;
  esac

  set +e
  output="$(BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: unmarked merge resolution exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $merge_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: gate did not identify unmarked merge resolution $merge_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge resolution: third-state unmarked merge commit was rejected"
}

run_stale_parent_merge_case() {
  local repo="$temp_root/stale-parent-merge"
  local old_state_sha
  local base_sha
  local old_tree
  local merge_sha
  local output
  local rc

  new_fixture_repo "$repo"
  printf 'historical marked state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'historical marked vector state\n\nFROZEN-VECTOR-CHANGE: fixture historical rationale'
  old_state_sha="$(git -C "$repo" rev-parse HEAD)"
  printf 'base state restored\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'restore vector before audited range\n\nFROZEN-VECTOR-CHANGE: fixture restore rationale'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  old_tree="$(git -C "$repo" rev-parse "${old_state_sha}^{tree}")"
  merge_sha="$(printf '%s\n' 'restore stale vector through redundant parent' \
    | git -C "$repo" commit-tree "$old_tree" -p "$base_sha" -p "$old_state_sha")"

  set +e
  output="$(BASE_SHA="$base_sha" HEAD_SHA="$merge_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: stale-parent merge exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $merge_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: gate missed stale-parent vector restoration $merge_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge provenance: stale parent could not restore vectors without a merge marker"
}

run_reverse_parent_range_case() {
  local repo="$temp_root/reverse-parent-range"
  local base_sha
  local output
  local rc

  new_fixture_repo "$repo"
  printf 'unrelated stale-branch commit\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm 'stale branch first-parent commit'
  git -C "$repo" switch -q main
  printf 'marked current default-branch state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked default-branch vector state\n\nFROZEN-VECTOR-CHANGE: fixture default rationale'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" switch -q feature
  git -C "$repo" merge -q --no-ff -s ours main -m 'reverse-parent merge retaining stale vectors'

  set +e
  output="$(BASE_SHA="$base_sha" DEFAULT_BRANCH=main "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: reverse-parent range exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"is not on audit head"*"first-parent chain"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: reverse-parent range did not fail closed on ambiguous lineage" >&2
      exit 1
      ;;
  esac
  echo "PASS merge range: event base outside first-parent chain failed closed"
}

run_disjoint_marked_merge_case() {
  local repo="$temp_root/disjoint-marked-merge"

  new_fixture_repo "$repo"
  printf 'base second vector\n' > "$repo/crates/calm-server/tests/vectors/second.json"
  git -C "$repo" add crates/calm-server/tests/vectors/second.json
  git -C "$repo" commit -qm $'add second vector fixture\n\nFROZEN-VECTOR-CHANGE: fixture setup rationale'
  git -C "$repo" update-ref refs/remotes/origin/main HEAD
  git -C "$repo" branch -f main HEAD
  git -C "$repo" switch -qc side
  printf 'marked side state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked first vector on side\n\nFROZEN-VECTOR-CHANGE: fixture side rationale'
  git -C "$repo" switch -q feature
  printf 'marked first-parent state\n' > "$repo/crates/calm-server/tests/vectors/second.json"
  git -C "$repo" commit -qam $'marked second vector on first parent\n\nFROZEN-VECTOR-CHANGE: fixture first-parent rationale'
  git -C "$repo" merge -q --no-ff side -m 'combine disjoint marked vector changes'

  BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS merge provenance: disjoint marked changes did not require a merge marker"
}

run_submodule_ignore_merge_case() {
  local repo="$temp_root/submodule-ignore-merge"
  local vector_path='crates/calm-server/tests/vectors'
  local initial_target
  local old_state_sha
  local base_sha
  local old_tree
  local merge_sha
  local output
  local rc

  new_fixture_repo "$repo"
  initial_target="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" rm -qr "$vector_path"
  printf '%s\n' \
    '[submodule "vectors"]' \
    "  path = $vector_path" \
    '  url = ./unused' \
    '  ignore = all' > "$repo/.gitmodules"
  git -C "$repo" add .gitmodules
  git -C "$repo" update-index --add --cacheinfo "160000,$initial_target,$vector_path"
  git -C "$repo" commit -qm $'convert vectors to historical gitlink\n\nFROZEN-VECTOR-CHANGE: fixture gitlink rationale'
  old_state_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" update-index --cacheinfo "160000,$old_state_sha,$vector_path"
  git -C "$repo" commit -qm $'advance vectors gitlink before range\n\nFROZEN-VECTOR-CHANGE: fixture gitlink advance rationale'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  old_tree="$(git -C "$repo" rev-parse "${old_state_sha}^{tree}")"
  merge_sha="$(printf '%s\n' 'restore stale gitlink with submodule ignore all' \
    | git -C "$repo" commit-tree "$old_tree" -p "$base_sha" -p "$old_state_sha")"
  mkdir -p "$repo/$vector_path"

  set +e
  output="$(BASE_SHA="$base_sha" HEAD_SHA="$merge_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: ignored-submodule merge exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $merge_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: submodule ignore hid merge $merge_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS diff configuration: submodule ignore could not hide a gitlink change"
}

run_literal_pathspec_merge_case() {
  local repo="$temp_root/literal-pathspec-merge"
  local vector_root="$repo/crates/calm-server/tests/vectors"

  new_fixture_repo "$repo"
  printf 'base literal-star file\n' > "$vector_root/star*.json"
  printf 'base wildcard-neighbor file\n' > "$vector_root/starX.json"
  git -C "$repo" add 'crates/calm-server/tests/vectors/star*.json' \
    crates/calm-server/tests/vectors/starX.json
  git -C "$repo" commit -qm $'add pathspec fixture vectors\n\nFROZEN-VECTOR-CHANGE: fixture setup rationale'
  git -C "$repo" update-ref refs/remotes/origin/main HEAD
  git -C "$repo" branch -f main HEAD
  git -C "$repo" switch -qc side
  printf 'marked literal-star side state\n' > "$vector_root/star*.json"
  git -C "$repo" commit -qam $'mark literal-star vector on side\n\nFROZEN-VECTOR-CHANGE: fixture side rationale'
  git -C "$repo" switch -q feature
  printf 'marked wildcard-neighbor first-parent state\n' > "$vector_root/starX.json"
  git -C "$repo" commit -qam $'mark wildcard-neighbor vector on first parent\n\nFROZEN-VECTOR-CHANGE: fixture first-parent rationale'
  git -C "$repo" merge -q --no-ff side -m 'combine pathspec-shaped vector files'

  BASE_SHA="$zero_sha" DEFAULT_BRANCH=main "$gate" "$repo" >/dev/null
  echo "PASS path handling: literal wildcard filename did not cause a merge false positive"
}

run_nested_reverse_parent_case() {
  local repo="$temp_root/nested-reverse-parent"
  local stale_sha
  local stale_tree
  local base_sha
  local inner_sha
  local head_sha
  local output
  local rc

  new_fixture_repo "$repo"
  stale_sha="$(git -C "$repo" rev-parse HEAD)"
  stale_tree="$(git -C "$repo" rev-parse "${stale_sha}^{tree}")"
  printf 'marked current base state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'mark current vector base\n\nFROZEN-VECTOR-CHANGE: fixture current rationale'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  inner_sha="$(printf '%s\n' 'inner reverse-parent stale merge' \
    | git -C "$repo" commit-tree "$stale_tree" -p "$stale_sha" -p "$base_sha")"
  head_sha="$(printf '%s\n' 'outer ordinary merge adopting stale state' \
    | git -C "$repo" commit-tree "$stale_tree" -p "$base_sha" -p "$inner_sha")"

  set +e
  output="$(BASE_SHA="$base_sha" HEAD_SHA="$head_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: nested reverse-parent merge exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $head_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: nested reverse-parent provenance hid outer merge $head_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge provenance: nested reverse-parent state restoration was rejected"
}

run_discarded_marker_laundering_case() {
  local repo="$temp_root/discarded-marker-laundering"
  local stale_sha
  local stale_tree
  local side_sha
  local base_sha
  local inner_sha
  local head_sha
  local output
  local rc

  new_fixture_repo "$repo"
  stale_sha="$(git -C "$repo" rev-parse HEAD)"
  stale_tree="$(git -C "$repo" rev-parse "${stale_sha}^{tree}")"
  git -C "$repo" switch -qc side
  printf 'marked state that will be discarded\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked decoy vector state\n\nFROZEN-VECTOR-CHANGE: fixture decoy rationale'
  side_sha="$(git -C "$repo" rev-parse HEAD)"
  inner_sha="$(printf '%s\n' 'discard marked decoy and retain stale state' \
    | git -C "$repo" commit-tree "$stale_tree" -p "$stale_sha" -p "$side_sha")"
  git -C "$repo" switch -q feature
  printf 'marked current base state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'mark current vector base\n\nFROZEN-VECTOR-CHANGE: fixture current rationale'
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  head_sha="$(printf '%s\n' 'restore stale state after discarded marked decoy' \
    | git -C "$repo" commit-tree "$stale_tree" -p "$base_sha" -p "$inner_sha")"

  set +e
  output="$(BASE_SHA="$base_sha" HEAD_SHA="$head_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" 2>&1)"
  rc=$?
  set -e

  if [ "$rc" -ne 1 ]; then
    printf '%s\n' "$output"
    echo "selftest failure: discarded-marker laundering exited $rc; expected 1" >&2
    exit 1
  fi
  case "$output" in
    *"commit $head_sha touches crates/calm-server/tests/vectors/ without"*) ;;
    *)
      printf '%s\n' "$output"
      echo "selftest failure: discarded marked state laundered outer merge $head_sha" >&2
      exit 1
      ;;
  esac
  echo "PASS merge provenance: discarded marked state could not launder a stale restoration"
}

run_recursive_merge_provenance_case() {
  local repo="$temp_root/recursive-merge-provenance"
  local base_sha
  local source_sha
  local source_tree
  local inner_sha
  local head_sha

  new_fixture_repo "$repo"
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" switch -qc source
  printf 'marked source state\n' > "$repo/crates/calm-server/tests/vectors/case.json"
  git -C "$repo" commit -qam $'marked source vector state\n\nFROZEN-VECTOR-CHANGE: fixture source rationale'
  source_sha="$(git -C "$repo" rev-parse HEAD)"
  source_tree="$(git -C "$repo" rev-parse "${source_sha}^{tree}")"
  inner_sha="$(printf '%s\n' 'inner ordinary merge adopting marked source' \
    | git -C "$repo" commit-tree "$source_tree" -p "$base_sha" -p "$source_sha")"
  head_sha="$(printf '%s\n' 'outer ordinary merge adopting justified inner state' \
    | git -C "$repo" commit-tree "$source_tree" -p "$base_sha" -p "$inner_sha")"

  BASE_SHA="$base_sha" HEAD_SHA="$head_sha" DEFAULT_BRANCH=main \
    "$gate" "$repo" >/dev/null
  echo "PASS merge provenance: justified state propagated through nested unmarked merges"
}

run_non_tip_violation_case
run_valid_multi_commit_case
run_unresolvable_base_case
run_default_branch_creation_case
run_invalid_nonzero_base_case
run_rewind_case
run_merge_history_violation_case
run_valid_merge_history_case
run_marked_discarded_side_case
run_default_branch_state_merge_case
run_unmarked_branch_merge_case
run_merge_resolution_violation_case
run_stale_parent_merge_case
run_reverse_parent_range_case
run_disjoint_marked_merge_case
run_submodule_ignore_merge_case
run_literal_pathspec_merge_case
run_nested_reverse_parent_case
run_discarded_marker_laundering_case
run_recursive_merge_provenance_case
echo "frozen-vector gate: fixtures passed"
