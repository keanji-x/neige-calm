#!/usr/bin/env bash

set -euo pipefail

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="${1:-$script_dir/../..}"
declare -A path_state_cache=()

error() {
  echo "::error::$*" >&2
  exit 1
}

commit_has_rationale() {
  local sha="$1"
  local message

  message="$(git -C "$repo_root" log -1 --format=%B "$sha")" \
    || error "failed to read commit message for $sha"
  case "$message" in
    *FROZEN-VECTOR-CHANGE:*) return 0 ;;
    *) return 1 ;;
  esac
}

# Return success only when COMMIT's state for one literal PATH was established
# by a marker-bearing transition outside the event base. Following the actual
# equal-state parent chain prevents a discarded marked change elsewhere in the
# DAG from laundering an unrelated stale state.
path_state_has_rationale() {
  local commit="$1"
  local path="$2"
  local key="${commit}"$'\034'"${path}"
  local cached="${path_state_cache[$key]-}"
  local reachable_rc
  local parents
  local first_parent
  local diff_rc
  local root_paths
  local root_rc
  local parent
  local parent_diff_rc
  local i
  local -a state_parents=()

  case "$cached" in
    yes) return 0 ;;
    no) return 1 ;;
  esac

  set +e
  git -C "$repo_root" merge-base --is-ancestor "$commit" "$base_sha" >/dev/null 2>&1
  reachable_rc=$?
  set -e
  case "$reachable_rc" in
    0)
      path_state_cache[$key]=no
      return 1
      ;;
    1) ;;
    *) error "failed to compare provenance commit $commit with event base $base_sha" ;;
  esac

  parents="$(git -C "$repo_root" log -1 --format=%P "$commit")" \
    || error "failed to read parents for provenance commit $commit"
  read -r -a state_parents <<< "$parents"

  if [ "${#state_parents[@]}" -eq 0 ]; then
    set +e
    root_paths="$(git -C "$repo_root" --literal-pathspecs diff-tree \
      --ignore-submodules=none --root --no-commit-id --name-only -r \
      "$commit" -- "$path" 2>&1)"
    root_rc=$?
    set -e
    if [ "$root_rc" -ne 0 ]; then
      printf '%s\n' "$root_paths" >&2
      error "failed to inspect root provenance commit $commit for '$path' (git diff-tree exit $root_rc)"
    fi
    if [ -n "$root_paths" ] && commit_has_rationale "$commit"; then
      path_state_cache[$key]=yes
      return 0
    fi
    path_state_cache[$key]=no
    return 1
  fi

  first_parent="${state_parents[0]}"
  set +e
  git -C "$repo_root" --literal-pathspecs diff --quiet --ignore-submodules=none \
    "$first_parent" "$commit" -- "$path"
  diff_rc=$?
  set -e
  case "$diff_rc" in
    0)
      if path_state_has_rationale "$first_parent" "$path"; then
        path_state_cache[$key]=yes
        return 0
      fi
      ;;
    1)
      if commit_has_rationale "$commit"; then
        path_state_cache[$key]=yes
        return 0
      fi
      for ((i = 1; i < ${#state_parents[@]}; i++)); do
        parent="${state_parents[$i]}"
        set +e
        git -C "$repo_root" --literal-pathspecs diff --quiet --ignore-submodules=none \
          "$parent" "$commit" -- "$path"
        parent_diff_rc=$?
        set -e
        case "$parent_diff_rc" in
          0)
            if path_state_has_rationale "$parent" "$path"; then
              path_state_cache[$key]=yes
              return 0
            fi
            ;;
          1) ;;
          *) error "failed to compare provenance commit $commit with parent $parent for '$path'" ;;
        esac
      done
      ;;
    *) error "failed to inspect provenance transition $first_parent..$commit for '$path'" ;;
  esac

  path_state_cache[$key]=no
  return 1
}

[ -d "$repo_root" ] || error "repository root does not exist: $repo_root"
repo_root="$(cd "$repo_root" && pwd)"
git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1 \
  || error "repository root is not a Git worktree: $repo_root"

vector_dir='crates/calm-server/tests/vectors/'
[ -d "$repo_root/$vector_dir" ] \
  || error "frozen-vector directory does not exist: $vector_dir"

head_rev="${HEAD_SHA:-HEAD}"
head_sha="$(git -C "$repo_root" rev-parse --verify --end-of-options "${head_rev}^{commit}" 2>/dev/null)" \
  || error "cannot resolve audit head '$head_rev' to a commit"

zero_sha=0000000000000000000000000000000000000000
requested_base="${BASE_SHA:-}"
base_sha=

if [ "$requested_base" = "$zero_sha" ]; then
  # On a branch-creation push, github.event.before is forty zeroes. The base
  # must cover the whole branch, so derive it from the repository's real
  # default branch instead of silently narrowing the audit to HEAD~1.
  [ -n "${DEFAULT_BRANCH:-}" ] \
    || error "BASE_SHA is absent or invalid, and DEFAULT_BRANCH is unavailable; refusing to narrow the audit range"
  default_ref="refs/remotes/origin/$DEFAULT_BRANCH"
  default_sha="$(git -C "$repo_root" rev-parse --verify --end-of-options "${default_ref}^{commit}" 2>/dev/null || true)"
  [ -n "$default_sha" ] \
    || error "BASE_SHA is absent or invalid, and default-branch ref '$default_ref' cannot be resolved"
  base_sha="$(git -C "$repo_root" merge-base "$default_sha" "$head_sha" 2>/dev/null || true)"
  [ -n "$base_sha" ] \
    || error "BASE_SHA is absent or invalid, and '$default_ref' has no merge-base with $head_sha"
  [ "$base_sha" != "$head_sha" ] \
    || error "default-branch merge-base equals audit head $head_sha; refusing an empty branch-creation audit range"
  echo "BASE_SHA is absent or invalid; auditing from merge-base $base_sha with $default_ref"
elif [ -z "$requested_base" ]; then
  error "BASE_SHA is absent; only the all-zero branch-creation sentinel may use the default-branch fallback"
else
  base_sha="$(git -C "$repo_root" rev-parse --verify --end-of-options "${requested_base}^{commit}" 2>/dev/null || true)"
  [ -n "$base_sha" ] \
    || error "BASE_SHA '$requested_base' cannot be resolved; refusing the branch-creation fallback for a nonzero base"
fi

git -C "$repo_root" merge-base --is-ancestor "$base_sha" "$head_sha" >/dev/null 2>&1 \
  || error "resolved base $base_sha is not an ancestor of audit head $head_sha; refusing a non-fast-forward audit range"

set +e
first_parent_history="$(git -C "$repo_root" rev-list --first-parent "$head_sha" 2>&1)"
first_parent_rc=$?
set -e
if [ "$first_parent_rc" -ne 0 ]; then
  printf '%s\n' "$first_parent_history" >&2
  error "failed to inspect first-parent history for audit head $head_sha (git rev-list exit $first_parent_rc)"
fi
base_on_first_parent=0
while IFS= read -r sha; do
  if [ "$sha" = "$base_sha" ]; then
    base_on_first_parent=1
  fi
done <<< "$first_parent_history"
[ "$base_on_first_parent" -eq 1 ] \
  || error "resolved base $base_sha is not on audit head $head_sha's first-parent chain; refusing an ambiguous merge range"

set +e
range_commits="$(git -C "$repo_root" rev-list "$base_sha..$head_sha" 2>&1)"
rev_list_rc=$?
set -e
if [ "$rev_list_rc" -ne 0 ]; then
  printf '%s\n' "$range_commits" >&2
  error "failed to enumerate commits in $base_sha..$head_sha (git rev-list exit $rev_list_rc)"
fi

bad=0
vector_commit_count=0
merge_paths_file="$(mktemp)" || error "failed to create merge-path scratch file"
trap 'rm -f -- "$merge_paths_file"' EXIT
while IFS= read -r sha; do
  [ -n "$sha" ] || continue

  parents="$(git -C "$repo_root" log -1 --format=%P "$sha")" \
    || error "failed to read parents for commit $sha"
  read -r -a parent_array <<< "$parents"

  if [ "${#parent_array[@]}" -lt 2 ]; then
    set +e
    touched_paths="$(git -C "$repo_root" --literal-pathspecs diff-tree \
      --ignore-submodules=none --root --no-commit-id --name-only -r \
      "$sha" -- "$vector_dir" 2>&1)"
    diff_tree_rc=$?
    set -e
    if [ "$diff_tree_rc" -ne 0 ]; then
      printf '%s\n' "$touched_paths" >&2
      error "failed to inspect frozen-vector changes for commit $sha (git diff-tree exit $diff_tree_rc)"
    fi
    [ -n "$touched_paths" ] || continue
  else
    first_parent="${parent_array[0]}"
    set +e
    git -C "$repo_root" --literal-pathspecs diff --ignore-submodules=none --name-only -z \
      "$first_parent" "$sha" -- "$vector_dir" > "$merge_paths_file"
    diff_rc=$?
    set -e
    if [ "$diff_rc" -ne 0 ]; then
      error "failed to inspect merge commit $sha against first parent $first_parent (git diff exit $diff_rc)"
    fi
    [ -s "$merge_paths_file" ] || continue

    # A merge may adopt vector changes already justified on another parent;
    # that ordinary composition does not need a duplicate marker. For every
    # path changed from parent 1, require both (a) a non-first parent with the
    # same resulting path state and (b) a marker-bearing change to that literal
    # path in the parent's history outside the event base. Without both, the
    # merge itself is introducing or restoring the path state.
    if ! commit_has_rationale "$sha"; then
      merge_has_provenance=1
      while IFS= read -r -d '' path; do
        path_has_provenance=0
        for ((i = 1; i < ${#parent_array[@]}; i++)); do
          parent="${parent_array[$i]}"
          set +e
          git -C "$repo_root" --literal-pathspecs diff --quiet --ignore-submodules=none \
            "$parent" "$sha" -- "$path"
          parent_diff_rc=$?
          set -e
          case "$parent_diff_rc" in
            0) ;;
            1) continue ;;
            *) error "failed to compare merge commit $sha with parent $parent for '$path' (git diff exit $parent_diff_rc)" ;;
          esac

          if path_state_has_rationale "$parent" "$path"; then
            path_has_provenance=1
          fi
        done
        if [ "$path_has_provenance" -ne 1 ]; then
          merge_has_provenance=0
        fi
      done < "$merge_paths_file"
      if [ "$merge_has_provenance" -eq 1 ]; then
        vector_commit_count=$((vector_commit_count + 1))
        continue
      fi
    fi
  fi

  vector_commit_count=$((vector_commit_count + 1))
  if ! commit_has_rationale "$sha"; then
    echo "::error::commit $sha touches $vector_dir without a 'FROZEN-VECTOR-CHANGE:' rationale in its message"
    git -C "$repo_root" log -1 --format='  %h %s' "$sha"
    bad=1
  fi
done <<< "$range_commits"

[ "$bad" -eq 0 ] || exit 1
[ "$vector_commit_count" -ne 0 ] \
  || { echo "OK: frozen vectors untouched in $base_sha..$head_sha"; exit 0; }
echo "OK: all vector-touching commits carry FROZEN-VECTOR-CHANGE:"
