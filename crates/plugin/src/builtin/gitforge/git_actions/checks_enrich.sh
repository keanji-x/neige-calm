# Structured Actions evidence only. No log download, third-party URL fetch, or raw log persistence.
# $1 trusted gh host, $2 canonical repository, $3 head, $4 shared sanitizer.
host=$1 repo=$2 head=$3 clean=$4
budget_end=$(($(date +%s) + 40))
gh_api() {
  remaining=$((budget_end - $(date +%s)))
  [ "$remaining" -gt 0 ] || return 1
  # Keep the kernel's process group; timeout the direct HTTP client, not a parent shell.
  # Bound the complete selected stream before any caller captures it. The final
  # status marker rejects both a truncated stream and a failed later page.
  response=$({ timeout --foreground "$remaining" gh api "$@"; api_status=$?;
    printf '\ngh-enrich-status:%s\n' "$api_status"; } | head -c 262208) || return 1
  [ "${response##*
}" = gh-enrich-status:0 ] || return 1
  response=${response%
gh-enrich-status:0}
  [ "$(printf '%s' "$response" | wc -c)" -le 262144 ] || return 1
  printf '%s\n' "$response"
}
IFS= read -r first || exit 1
prefix=
case "$first" in
  settle*|wait*) prefix=$first; IFS= read -r document || exit 1 ;;
  *) document=$first ;;
esac
case "$host" in ''|*[!a-zA-Z0-9.-]*) exit 1 ;; esac
if [ "$host" = github.com ]; then api=https://api.github.com; else api="https://$host/api/v3"; fi
unavailable() {
  jq -nc --arg reason "$1" '{status:"unavailable",reason:$reason}'
}
diagnose() {
  check=$1
  check_id=$(printf '%s\n' "$check" | jq -r '._source.databaseId // empty')
  run_id=$(printf '%s\n' "$check" | jq -r '._source.checkSuite.workflowRun.databaseId // empty')
  suite_id=$(printf '%s\n' "$check" | jq -r '._source.checkSuite.databaseId // empty')
  case "$check_id:$run_id:$suite_id" in *[!0-9:]*|:*|*::*|*:) unavailable 'not an Actions check'; return ;; esac
  node_id=$(printf '%s\n' "$check" | jq -r '._source.id')
  run=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{id,head_sha,run_attempt,check_suite_id,repository:{full_name:.repository.full_name}}') || {
    unavailable 'Actions run unavailable'; return;
  }
  if ! printf '%s\n' "$run" | jq -e --arg repo "$repo" --arg head "$head" --argjson run "$run_id" --argjson suite "$suite_id" \
    '.id == $run and .head_sha == $head and .repository.full_name == $repo and .check_suite_id == $suite and (.run_attempt | type == "number" and . > 0)' >/dev/null; then
    unavailable 'Actions run identity mismatch'; return
  fi
  attempt=$(printf '%s\n' "$run" | jq -r .run_attempt)
  detail=$(gh_api --hostname "$host" "repos/$repo/check-runs/$check_id" --jq "$clean {id,node_id,head_sha,status,check_suite:{id:.check_suite.id},app:{slug:.app.slug},summary:([.output.summary,.output.text] | map(select(type == \"string\" and length > 0) | clean) | join(\" \") | .[0:1000])}") || {
    unavailable 'check output unavailable'; return;
  }
  if ! printf '%s\n' "$detail" | jq -e --arg head "$head" --arg node "$node_id" --argjson check "$check_id" --argjson suite "$suite_id" \
    '.id == $check and .node_id == $node and .head_sha == $head and .check_suite.id == $suite and .app.slug == "github-actions" and .status == "completed"' >/dev/null; then
    unavailable 'check identity mismatch or non-Actions check'; return
  fi
  # Select by the exact authenticated API URL, never a name, a detailsUrl tail, or latest run.
  pages=$(gh_api --hostname "$host" --paginate "repos/$repo/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100" \
    --jq "$clean [.jobs[] | {id,run_id,run_attempt,head_sha,run_url,check_run_url,status,steps:[.steps[]? | select(.conclusion == \"failure\") | .name | select(type == \"string\") | clean | select(test(\"\\\\S\"))][0:8]}]") || {
    unavailable 'Actions jobs unavailable'; return;
  }
  jobs=$(printf '%s\n' "$pages" | jq -sc --arg check_url "$api/repos/$repo/check-runs/$check_id" 'add | map(select(.check_run_url == $check_url))') || {
    unavailable 'Actions jobs could not be parsed'; return;
  }
  if ! printf '%s\n' "$jobs" | jq -e --arg head "$head" --arg run_url "$api/repos/$repo/actions/runs/$run_id" --argjson run "$run_id" --argjson attempt "$attempt" \
    'length == 1 and .[0].run_id == $run and .[0].run_attempt == $attempt and .[0].head_sha == $head and .[0].run_url == $run_url and .[0].status == "completed" and (.[0].id | type == "number" and . > 0)' >/dev/null; then
    unavailable 'Actions job identity mismatch or job unavailable'; return
  fi
  summary=$(printf '%s\n' "$detail" | jq -r .summary)
  # Test identities are a closed protocol emitted by our nextest JUnit renderer.
  # Always read annotations: Actions' generic exit-code summary is not test evidence.
  annotations='[]'
  if pages=$(gh_api --hostname "$host" --paginate "repos/$repo/check-runs/$check_id/annotations?per_page=100" \
    --jq "$clean [.[] | select(.annotation_level == \"failure\") | {title:(.title // \"\" | clean),message:(.message // \"\" | clean)}][0:16]"); then
    annotations=$(printf '%s\n' "$pages" | jq -sc 'add[0:16]') || {
      unavailable 'check annotations could not be parsed'; return;
    }
  elif [ -z "$summary" ]; then
    unavailable 'check annotations unavailable'; return
  fi
  tests=$(printf '%s\n' "$annotations" | jq -c '[.[] | .title | select(startswith("nextest failure: ")) | ltrimstr("nextest failure: ") | select(length <= 223 and test("^.+::.+$"))] | unique') || {
    unavailable 'check annotations could not be parsed'; return;
  }
  if [ "$tests" != '[]' ]; then
    summary=$(printf '%s\n' "$annotations" | jq -r '[.[] | select(.title | startswith("nextest failure: ")) | .message | select(test("\\S"))] | join(" ") | .[0:1000]')
  elif [ -z "$summary" ]; then
    summary=$(printf '%s\n' "$annotations" | jq -r '[.[0:4][].message] | join(" ") | .[0:1000]')
  fi
  if ! printf '%s' "$summary" | jq -Rse 'test("\\S")' >/dev/null || ! printf '%s\n' "$jobs" | jq -e '.[0].steps | any(test("\\S"))' >/dev/null; then
    unavailable 'no supported failure steps and error summary'; return
  fi
  # Re-running a workflow while enrichment is in progress invalidates its job evidence.
  current=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{head_sha,run_attempt}') || {
    unavailable 'Actions run unavailable after enrichment'; return;
  }
  if ! printf '%s\n' "$current" | jq -e --arg head "$head" --argjson attempt "$attempt" '.head_sha == $head and .run_attempt == $attempt' >/dev/null; then
    unavailable 'Actions attempt changed during enrichment'; return
  fi
  job_id=$(printf '%s\n' "$jobs" | jq -r '.[0].id')
  printf '%s\n' "$jobs" | jq -c --argjson tests "$tests" --arg summary "$summary" --arg url "https://$host/$repo/actions/runs/$run_id/job/$job_id" \
    '{status:"available",failed_tests:$tests,failed_steps:.[0].steps,error_summary:$summary,log_url:$url,truncated:true}'
}
diagnostics=$(printf '%s\n' "$document" | jq -c '.failed_checks[]' | (
  count=0
  while IFS= read -r check; do
    count=$((count + 1))
    if [ "$count" -le 16 ]; then diagnose "$check"; else unavailable 'diagnostic output budget exceeded'; fi
  done
) | jq -sc '.') || exit 1
result=$(printf '%s\n' "$document" | jq -c --argjson diagnostics "$diagnostics" "$clean \
  .failed_checks |= (to_entries | map(.value | clean_check) as \$checks | \
    [range(0; (\$checks|length)) as \$i | \$checks[\$i] + {diagnostics:\$diagnostics[\$i]}])") || exit 1
[ -z "$prefix" ] || printf '%s\n' "$prefix"
printf '%s\n' "$result"
