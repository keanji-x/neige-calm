# Structured Actions evidence only. No log download, third-party URL fetch, or raw log persistence.
# $1 trusted gh host, $2 canonical repository, $3 head, $4 shared sanitizer. Runs after the
# shared Actions prelude, which defines gh_api.
host=$1 repo=$2 head=$3 clean=$4
budget_end=$(($(date +%s) + 40))
IFS= read -r first || exit 1
prefix=
case "$first" in
  settle*|wait*) prefix=$first; IFS= read -r document || exit 1 ;;
  *) document=$first ;;
esac
case "$host" in ''|*[!a-zA-Z0-9.-]*) exit 1 ;; esac
if [ "$host" = github.com ]; then api=https://api.github.com; else api="https://$host/api/v3"; fi
# $1 reason; $2 the status of the failed gh_api call it reports, if any.
unavailable() {
  [ "${2:-1}" -ne 2 ] || set -- 'diagnostic time budget exhausted'
  jq -nc --arg reason "$1" '{status:"unavailable",reason:$reason}'
}
# Prints `check_id:run_id:suite_id` of an Actions check; fails for any other check.
actions_ids() {
  ids=$(printf '%s\n' "$1" | jq -r '._source | [.databaseId, .checkSuite.workflowRun.databaseId, .checkSuite.databaseId] | map(. // "" | tostring) | join(":")')
  case "$ids" in *[!0-9:]*|:*|*::*|*:) return 1 ;; esac
  printf '%s\n' "$ids"
}
# One run's identity and its attempt's jobs, shared by every failed check of the run:
# {run, jobs}, or {diag} when they are unavailable.
read_run() {
  run_id=$1
  run=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{id,head_sha,run_attempt,check_suite_id,repository:{full_name:.repository.full_name}}') || {
    unavailable 'Actions run unavailable' $? | jq -c '{diag:.}'; return;
  }
  if ! printf '%s\n' "$run" | jq -e --arg repo "$repo" --arg head "$head" --argjson run "$run_id" \
    '.id == $run and .head_sha == $head and .repository.full_name == $repo and (.run_attempt | type == "number" and . > 0)' >/dev/null; then
    unavailable 'Actions run identity mismatch' | jq -c '{diag:.}'; return
  fi
  attempt=$(printf '%s\n' "$run" | jq -r .run_attempt)
  # steps_cut: more failed steps than the 8 reported, or a reported name cut by the sanitizer.
  pages=$(gh_api --hostname "$host" --paginate "repos/$repo/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100" \
    --jq "$clean [.jobs[] | {id,run_id,run_attempt,head_sha,run_url,check_run_url,status} + ([.steps[]? | select(.conclusion == \"failure\") | .name | select(type == \"string\") | {name:clean,cut:(clean_uncut | length > 1000)} | select(.name | test(\"\\\\S\"))] | {steps:[.[0:8][].name],steps_cut:(length > 8 or any(.[0:8][]; .cut))})]") || {
    unavailable 'Actions jobs unavailable' $? | jq -c '{diag:.}'; return;
  }
  { printf '%s\n' "$run"; printf '%s\n' "$pages" | jq -sc 'add'; } | jq -sc '{run:.[0],jobs:.[1]}' || {
    unavailable 'Actions jobs could not be parsed' | jq -c '{diag:.}';
  }
}
# Diagnoses one check from its run's shared evidence. An available result carries `_run` until
# that run's closing attempt re-check.
diagnose() {
  check=$1
  ids=$(actions_ids "$check") || { unavailable 'not an Actions check'; return; }
  check_id=${ids%%:*}
  suite_id=${ids##*:}
  run_id=${ids#*:}
  run_id=${run_id%%:*}
  state=$(printf '%s\n' "$runs" | jq -c --arg id "$run_id" '.[$id]')
  if printf '%s\n' "$state" | jq -e 'has("diag")' >/dev/null; then
    printf '%s\n' "$state" | jq -c .diag; return
  fi
  if ! printf '%s\n' "$state" | jq -e --argjson suite "$suite_id" '.run.check_suite_id == $suite' >/dev/null; then
    unavailable 'Actions run identity mismatch'; return
  fi
  attempt=$(printf '%s\n' "$state" | jq -r .run.run_attempt)
  node_id=$(printf '%s\n' "$check" | jq -r '._source.id')
  conclusion=$(printf '%s\n' "$check" | jq -r "$clean ._source.conclusion // \"\" | clean")
  detail=$(gh_api --hostname "$host" "repos/$repo/check-runs/$check_id" --jq "$clean {id,node_id,head_sha,status,check_suite:{id:.check_suite.id},app:{slug:.app.slug}} + ([.output.summary,.output.text] | map(select(type == \"string\" and length > 0) | clean_uncut) | join(\" \") | {summary:.[0:1000],summary_cut:(length > 1000)})") || {
    unavailable 'check output unavailable' $?; return;
  }
  if ! printf '%s\n' "$detail" | jq -e --arg head "$head" --arg node "$node_id" --argjson check "$check_id" --argjson suite "$suite_id" \
    '.id == $check and .node_id == $node and .head_sha == $head and .check_suite.id == $suite and .app.slug == "github-actions" and .status == "completed"' >/dev/null; then
    unavailable 'check identity mismatch or non-Actions check'; return
  fi
  # Select by the exact authenticated API URL, never a name, a detailsUrl tail, or latest run.
  jobs=$(printf '%s\n' "$state" | jq -c --arg check_url "$api/repos/$repo/check-runs/$check_id" '.jobs | map(select(.check_run_url == $check_url))') || {
    unavailable 'Actions jobs could not be parsed'; return;
  }
  if ! printf '%s\n' "$jobs" | jq -e --arg head "$head" --arg run_url "$api/repos/$repo/actions/runs/$run_id" --argjson run "$run_id" --argjson attempt "$attempt" \
    'length == 1 and .[0].run_id == $run and .[0].run_attempt == $attempt and .[0].head_sha == $head and .[0].run_url == $run_url and .[0].status == "completed" and (.[0].id | type == "number" and . > 0)' >/dev/null; then
    unavailable 'Actions job identity mismatch or job unavailable'; return
  fi
  summary=$(printf '%s\n' "$detail" | jq -r .summary)
  # Test identities and the `truncated:` warning are a closed protocol emitted by our nextest
  # JUnit renderer (scripts/ci/nextest-failure-annotations.py).
  # Always read annotations: Actions' generic exit-code summary is not test evidence.
  annotations='{"failures":[],"cut":false}'
  if pages=$(gh_api --hostname "$host" --paginate "repos/$repo/check-runs/$check_id/annotations?per_page=100" \
    --jq "$clean [.[] | select(.annotation_level == \"failure\") | {title:(.title // \"\" | clean),message:(.message // \"\" | clean),message_cut:(.message // \"\" | clean_uncut | length > 1000)}] as \$failures | {failures:\$failures[0:16],count:(\$failures | length),producer_cut:any(.[]; .annotation_level == \"warning\" and .title == \"nextest JUnit diagnostics\" and (.message | type == \"string\" and startswith(\"truncated:\")))}"); then
    # cut: more failure annotations than the 16 kept, or the producer reported its own cut.
    annotations=$(printf '%s\n' "$pages" | jq -sc '{failures:(map(.failures) | add // [] | .[0:16]),cut:((map(.count) | add // 0) > 16 or any(.[]; .producer_cut))}') || {
      unavailable 'check annotations could not be parsed'; return;
    }
  else
    status=$?
    if [ "$status" -eq 2 ] || [ -z "$summary" ]; then unavailable 'check annotations unavailable' "$status"; return; fi
  fi
  tests=$(printf '%s\n' "$annotations" | jq -c '[.failures[] | .title | select(startswith("nextest failure: ")) | ltrimstr("nextest failure: ") | select(length <= 223 and test("^.+::.+$"))] | unique') || {
    unavailable 'check annotations could not be parsed'; return;
  }
  # The reported summary and whether a bound cut it: the check output's, else annotation messages.
  if [ "$tests" != '[]' ]; then
    selected='[.failures[] | select(.title | startswith("nextest failure: ")) | select(.message | test("\\S"))]'
  elif [ -z "$summary" ]; then
    selected='(.failures | (length > 4) as $cut | .[0:4] | map(.message_cut = (.message_cut or $cut)))'
  else
    selected=
  fi
  if [ -n "$selected" ]; then
    reported=$(printf '%s\n' "$annotations" | jq -c "$selected | (map(.message) | join(\" \")) as \$text | {summary:\$text[0:1000],summary_cut:(any(.[]; .message_cut) or (\$text | length > 1000))}")
  else
    reported=$(printf '%s\n' "$detail" | jq -c '{summary,summary_cut}')
  fi
  summary=$(printf '%s\n' "$reported" | jq -r .summary)
  if ! printf '%s\n' "$jobs" | jq -e '.[0].steps | any(test("\\S"))' >/dev/null; then
    # A cancelled or timed-out job often stops before any step fails.
    case "$conclusion" in
      ''|FAILURE) ;;
      *) unavailable "check concluded $conclusion without a failed step"; return ;;
    esac
  fi
  if ! printf '%s' "$summary" | jq -Rse 'test("\\S")' >/dev/null || ! printf '%s\n' "$jobs" | jq -e '.[0].steps | any(test("\\S"))' >/dev/null; then
    unavailable 'no supported failure steps and error summary'; return
  fi
  job_id=$(printf '%s\n' "$jobs" | jq -r '.[0].id')
  # truncated: a bound cut reported evidence. The full log behind log_url never counts.
  { printf '%s\n' "$jobs"; printf '%s\n' "$annotations"; printf '%s\n' "$reported"; } | jq -sc --argjson tests "$tests" --arg url "https://$host/$repo/actions/runs/$run_id/job/$job_id" --arg run "$run_id" \
    '.[0][0] as $job | {status:"available",failed_tests:$tests,failed_steps:$job.steps,error_summary:.[2].summary,log_url:$url,truncated:($job.steps_cut or .[1].cut or .[2].summary_cut),_run:$run}'
}
# Diagnose at most 16 failed checks. Each Actions run is read, its attempt's jobs listed and
# its attempt re-checked once, however many of its jobs failed.
selected_checks=$(printf '%s\n' "$document" | jq -c '.failed_checks[0:16][]') || exit 1
runs='{}'
while IFS= read -r check; do
  ids=$(actions_ids "$check") || continue
  run_id=${ids#*:}
  run_id=${run_id%%:*}
  if printf '%s\n' "$runs" | jq -e --arg id "$run_id" 'has($id)' >/dev/null; then continue; fi
  runs=$({ printf '%s\n' "$runs"; read_run "$run_id"; } | jq -sc --arg id "$run_id" '.[0] + {($id): .[1]}') || exit 1
done <<EOF
$selected_checks
EOF
entries=$(printf '%s\n' "$document" | jq -c '.failed_checks[]' | (
  count=0
  while IFS= read -r check; do
    count=$((count + 1))
    if [ "$count" -le 16 ]; then diagnose "$check"; else unavailable 'diagnostic output budget exceeded'; fi
  done
)) || exit 1
# Re-running a workflow while enrichment is in progress invalidates its job evidence.
for run_id in $(printf '%s\n' "$entries" | jq -r '._run // empty' | sort -u); do
  attempt=$(printf '%s\n' "$runs" | jq -r --arg id "$run_id" '.[$id].run.run_attempt')
  stale=
  current=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{head_sha,run_attempt}') ||
    stale=$(unavailable 'Actions run unavailable after enrichment' $?)
  if [ -z "$stale" ] && ! printf '%s\n' "$current" | jq -e --arg head "$head" --argjson attempt "$attempt" '.head_sha == $head and .run_attempt == $attempt' >/dev/null; then
    stale=$(unavailable 'Actions attempt changed during enrichment')
  fi
  [ -z "$stale" ] || entries=$(printf '%s\n' "$entries" | jq -c --arg id "$run_id" --argjson stale "$stale" 'if ._run == $id then $stale else . end') || exit 1
done
result=$({ printf '%s\n' "$document"; printf '%s\n' "$entries" | jq -sc 'map(del(._run))'; } | jq -sc "$clean \
  .[1] as \$diagnostics | .[0] | .failed_checks |= (to_entries | map(.value | clean_check) as \$checks | \
    [range(0; (\$checks|length)) as \$i | \$checks[\$i] + {diagnostics:\$diagnostics[\$i]}])") || exit 1
[ -z "$prefix" ] || printf '%s\n' "$prefix"
printf '%s\n' "$result"
