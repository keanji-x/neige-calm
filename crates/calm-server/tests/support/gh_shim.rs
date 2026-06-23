use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn write_gh_shim(dir: &Path) {
    let path = dir.join("gh");
    std::fs::write(&path, GH_SHIM).expect("write gh shim");
    let mut perms = std::fs::metadata(&path)
        .expect("gh shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod gh shim");
}

pub const GH_SHIM: &str = r#"#!/bin/sh
# Hermetic gh shim for forge_workflow_e2e.
# State is derived only from --repo so the kernel's env-cleared subprocess can
# replay probes without test-only variables. The merge command is idempotent:
# repeated merges for the same PR return the original recorded merge oid.

get_arg() {
  wanted=$1
  shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$wanted" ]; then
      shift
      if [ "$#" -gt 0 ]; then
        printf '%s\n' "$1"
        return 0
      fi
      return 1
    fi
    shift
  done
  return 1
}

state_dir_for() {
  printf '%s.shimstate\n' "$1"
}

ensure_state() {
  repo=$1
  state=$(state_dir_for "$repo")
  mkdir -p "$state/prs" "$state/issues"
  printf '%s\n' "$state"
}

inc_counter() {
  file=$1
  if [ -f "$file" ]; then
    count=$(cat "$file")
  else
    count=0
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$file"
}

block_if_requested() {
  state=$1
  verb=$2
  block="$state/block_$verb"
  release="$state/release_$verb"
  [ -f "$block" ] || return 0
  i=0
  while [ "$i" -lt 200 ]; do
    [ -f "$release" ] && return 0
    # CI uses GNU coreutils; keep a real delay if fractional sleep is unavailable.
    sleep 0.1 2>/dev/null || sleep 1
    i=$((i + 1))
  done
  return 0
}

find_pr() {
  selector=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    number=$(cat "$pr_dir/number")
    head=$(cat "$pr_dir/head")
    if [ "$selector" = "$number" ] || [ "$selector" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

find_pr_by_head() {
  wanted_head=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    head=$(cat "$pr_dir/head")
    if [ "$wanted_head" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

print_pr_json() {
  pr_dir=$1
  number=$(cat "$pr_dir/number")
  head_sha=$(cat "$pr_dir/headRefOid")
  printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
}

[ "$#" -ge 2 ] || {
  echo "unsupported gh invocation" >&2
  exit 2
}

area=$1
verb=$2
shift 2

case "$area:$verb" in
  pr:list)
    repo=$(get_arg --repo "$@") || exit 2
    base=$(get_arg --base "$@" || true)
    head=$(get_arg --head "$@" || true)
    state=$(ensure_state "$repo")
    printf '['
    sep=
    for pr_dir in "$state"/prs/*; do
      [ -d "$pr_dir" ] || continue
      merged=$(cat "$pr_dir/merged")
      pr_base=$(cat "$pr_dir/base")
      pr_head=$(cat "$pr_dir/head")
      if [ "$merged" = "true" ]; then
        continue
      fi
      if [ -n "$base" ] && [ "$base" != "$pr_base" ]; then
        continue
      fi
      if [ -n "$head" ] && [ "$head" != "$pr_head" ]; then
        continue
      fi
      number=$(cat "$pr_dir/number")
      printf '%s%s' "$sep" "$number"
      sep=,
    done
    printf ']\n'
    ;;
  pr:create)
    repo=$(get_arg --repo "$@") || exit 2
    head=$(get_arg --head "$@") || exit 2
    base=$(get_arg --base "$@") || exit 2
    state=$(ensure_state "$repo")
    if pr_dir=$(find_pr_by_head "$head" "$state"); then
      print_pr_json "$pr_dir"
      exit 0
    fi
    next_file="$state/next_pr"
    if [ -f "$next_file" ]; then
      number=$(cat "$next_file")
    else
      number=1
    fi
    next=$((number + 1))
    printf '%s\n' "$next" > "$next_file"
    head_sha=$(git --git-dir "$repo" rev-parse "$head")
    pr_dir="$state/prs/$number"
    mkdir -p "$pr_dir"
    printf '%s\n' "$number" > "$pr_dir/number"
    printf '%s\n' "$head" > "$pr_dir/head"
    printf '%s\n' "$base" > "$pr_dir/base"
    printf '%s\n' "$head_sha" > "$pr_dir/headRefOid"
    printf 'false\n' > "$pr_dir/merged"
    print_pr_json "$pr_dir"
    ;;
  pr:diff)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    base=$(cat "$pr_dir/base")
    head=$(cat "$pr_dir/head")
    patch_file="$state/diff.$$"
    if git --git-dir "$repo" diff --patch "$base...$head" > "$patch_file" && [ -s "$patch_file" ]; then
      cat "$patch_file"
    else
      printf 'diff --git a/feature.txt b/feature.txt\n'
      printf 'new file mode 100644\n'
      printf '--- /dev/null\n'
      printf '+++ b/feature.txt\n'
      printf '@@ -0,0 +1 @@\n'
      printf '+hello from e2e\n'
    fi
    rm -f "$patch_file"
    ;;
  pr:view)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    number=$(cat "$pr_dir/number")
    head_sha=$(cat "$pr_dir/headRefOid")
    merged=$(cat "$pr_dir/merged")
    case "$json_fields" in
      state)
        if [ "$merged" = "true" ]; then
          printf '{"state":"MERGED"}\n'
        else
          printf '{"state":"OPEN"}\n'
        fi
        ;;
      number,headRefOid)
        printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
        ;;
      headRefOid,mergeCommit)
        if [ "$merged" = "true" ]; then
          merge_sha=$(cat "$pr_dir/merge_sha")
          printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
        else
          printf '{"headRefOid":"%s","mergeCommit":null}\n' "$head_sha"
        fi
        ;;
      statusCheckRollup)
        printf '{"conclusion":"success"}\n'
        ;;
      *)
        echo "unsupported gh pr view --json $json_fields" >&2
        exit 2
        ;;
    esac
    ;;
  pr:merge)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    expected_head=$(get_arg --match-head-commit "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    head_sha=$(cat "$pr_dir/headRefOid")
    if [ -n "$expected_head" ] && [ "$expected_head" != "$head_sha" ]; then
      echo "head commit did not match" >&2
      exit 1
    fi
    if [ "$(cat "$pr_dir/merged")" = "true" ]; then
      merge_sha=$(cat "$pr_dir/merge_sha")
    else
      number=$(cat "$pr_dir/number")
      merge_sha=$(printf '%s' "merge:$number:$head_sha" | git hash-object --stdin)
      printf '%s\n' "$merge_sha" > "$pr_dir/merge_sha"
      printf 'true\n' > "$pr_dir/merged"
      inc_counter "$state/pr_merge_count"
      block_if_requested "$state" pr_merge
    fi
    printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
    ;;
  issue:view)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    jq_expr=$(get_arg --jq "$@" || true)
    state=$(ensure_state "$repo")
    issue_state=OPEN
    if [ -f "$state/issues/$issue.closed" ]; then
      issue_state=CLOSED
    fi
    case "$json_fields:$jq_expr" in
      state:*)
        printf '{"state":"%s"}\n' "$issue_state"
        ;;
      body:.body)
        printf '# Issue %s\n\nFake issue body for issue-development ingestion.\n' "$issue"
        ;;
      *)
        echo "unsupported gh issue view --json $json_fields --jq $jq_expr" >&2
        exit 2
        ;;
    esac
    ;;
  issue:close)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    if [ ! -f "$state/issues/$issue.closed" ]; then
      printf 'closed\n' > "$state/issues/$issue.closed"
      inc_counter "$state/issue_close_count"
      block_if_requested "$state" issue_close
    fi
    printf 'closed issue %s\n' "$issue"
    ;;
  *)
    echo "unsupported gh invocation: $area $verb" >&2
    exit 2
    ;;
esac
"#;
