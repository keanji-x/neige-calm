# Bounded failure evidence of one head's GitHub Actions runs, read on demand. Only this read
# downloads job logs; the forge.pr.checks wake stays log-free (#2409). Runs after the shared
# Actions prelude. $1 repository selector, $2 head SHA, $3 shared sanitizer.
# Prints {head_sha, runs, truncated}: at most 4 completed, unsuccessful runs of exactly this head
# and repository, at most 8 failed, timed-out or cancelled jobs of their attempts, and per job
# the sanitized last <= 200 lines (<= 16 KiB JSON-encoded) of its log or why it is unavailable.
head=$2 clean=$3
budget_end=$(($(date +%s) + 120))
resolve_repo "$1" || exit 1
case "$host" in ''|*[!a-zA-Z0-9.-]*) exit 1 ;; esac
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
# $1 reason: an unavailable job log; job metadata is kept.
log_unavailable() {
  jq -nc --arg reason "$1" '{status:"unavailable",reason:$reason}'
}
# $1 job id. Streams the log through a 256 KiB tail under a 20 s timeout, then keeps the last
# lines within the bounds, each sanitized on its own so line breaks survive.
read_log() {
  remaining=$((budget_end - $(date +%s)))
  [ "$remaining" -gt 0 ] || { log_unavailable 'time budget exhausted'; return; }
  [ "$remaining" -le 20 ] || remaining=20
  { timeout --foreground "$remaining" gh api --hostname "$host" "repos/$repo/actions/jobs/$1/logs" </dev/null 2>/dev/null
    printf '%s\n' "$?" > "$tmp/status"; } | tail -c 262145 > "$tmp/log" || { log_unavailable 'log download failed'; return; }
  case "$(cat "$tmp/status")" in
    0) ;;
    124) log_unavailable 'log download timed out'; return ;;
    *) log_unavailable 'log download failed (HTTP error or no access)'; return ;;
  esac
  # More than 256 KiB arrived: the first kept line may be partial, so it is dropped.
  if [ "$(wc -c < "$tmp/log")" -gt 262144 ]; then cut=true; else cut=false; fi
  # A line over 4096 characters is replaced whole: a cut could split a secret before redaction.
  jq -Rnc --argjson cut "$cut" "$clean"'
    [inputs] | (if $cut then .[1:] else . end) as $all | $all[-200:]
    | map(if length > 4096 then {text:"[line omitted: longer than 4096 characters]",cut:true}
          else clean_uncut | {text:.[0:1000],cut:(length > 1000)} end)
    | reduce reverse[] as $line ({kept:[],bytes:0,full:false};
        if .full then . else ($line.text | tojson | utf8bytelength) as $n
          | if .bytes + $n > 16384 then .full = true else .kept = [$line] + .kept | .bytes += $n end end)
    | {status:"available",text:(.kept | map(.text) | join("\n")),
       truncated:($cut or ($all | length) > (.kept | length) or any(.kept[]; .cut))}' "$tmp/log" ||
    log_unavailable 'log could not be read'
}
runs=$(gh_api --hostname "$host" --paginate "repos/$repo/actions/runs?head_sha=$head&per_page=100" \
  --jq '[.workflow_runs[] | {id,head_sha,run_attempt,status,conclusion,name,html_url,repository:{full_name:.repository.full_name}}]') || exit 1
runs=$(printf '%s\n' "$runs" | jq -sc --arg head "$head" --arg repo "$repo" '
  add // [] | map(select(.head_sha == $head and .repository.full_name == $repo and .status == "completed"
    and (.conclusion | type == "string") and .conclusion != "success" and .conclusion != "neutral" and .conclusion != "skipped"
    and (.id | type == "number" and . > 0) and (.run_attempt | type == "number" and . > 0)))
  | {runs:.[0:4], cut:(length > 4)}') || exit 1
truncated=$(printf '%s\n' "$runs" | jq -r .cut)
jobs_left=8
entries=
run_count=$(printf '%s\n' "$runs" | jq '.runs | length')
i=0
while [ "$i" -lt "$run_count" ]; do
  run=$(printf '%s\n' "$runs" | jq -c --argjson i "$i" '.runs[$i]')
  i=$((i + 1))
  run_id=$(printf '%s\n' "$run" | jq -r .id)
  attempt=$(printf '%s\n' "$run" | jq -r .run_attempt)
  if [ "$jobs_left" -le 0 ]; then
    jobs='{"status":"unavailable","reason":"job budget of 8 used by earlier runs","jobs":[],"cut":true}'
  elif pages=$(gh_api --hostname "$host" --paginate "repos/$repo/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100" \
    --jq '[.jobs[] | {id,run_id,run_attempt,head_sha,name,conclusion,html_url,steps:[.steps[]? | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "cancelled") | {number,name}]}]'); then
    jobs=$(printf '%s\n' "$pages" | jq -sc --arg head "$head" --argjson run "$run_id" --argjson attempt "$attempt" --argjson left "$jobs_left" "$clean"'
      add // [] | map(select(.run_id == $run and .run_attempt == $attempt and .head_sha == $head
        and (.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "cancelled")
        and (.id | type == "number" and . > 0)))
      | {status:"available", cut:(length > $left or any(.[0:$left][]; .steps | length > 8)),
         jobs:[.[0:$left][] | {id, name:(.name // "" | clean | .[0:200]), conclusion,
           failed_steps:[.steps[0:8][] | {number, name:(.name // "" | clean | .[0:200])}],
           html_url:(.html_url // "" | clean)}]}') ||
      jobs='{"status":"unavailable","reason":"Actions jobs could not be parsed","jobs":[],"cut":false}'
  else
    status=$?
    reason='Actions jobs unavailable'
    [ "$status" -ne 2 ] || reason='time budget exhausted'
    jobs=$(jq -nc --arg reason "$reason" '{status:"unavailable",reason:$reason,jobs:[],cut:false}')
  fi
  jobs_left=$((jobs_left - $(printf '%s\n' "$jobs" | jq '.jobs | length')))
  logged='[]'
  for job_id in $(printf '%s\n' "$jobs" | jq -r '.jobs[].id'); do
    log=$(read_log "$job_id")
    logged=$({ printf '%s\n' "$logged"; printf '%s\n' "$log"; } | jq -sc --argjson id "$job_id" '.[0] + [{id:$id,log:.[1]}]') || exit 1
  done
  # Re-running the workflow during the read makes its evidence stale.
  if current=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{head_sha,run_attempt}') &&
    printf '%s\n' "$current" | jq -e --arg head "$head" --argjson attempt "$attempt" '.head_sha == $head and .run_attempt == $attempt' >/dev/null; then
    stale=false
  else
    stale=true
  fi
  entry=$({ printf '%s\n' "$run"; printf '%s\n' "$jobs"; printf '%s\n' "$logged"; } | jq -sc --argjson stale "$stale" "$clean"'
    .[0] as $run | .[1] as $jobs | (.[2] | map({(.id | tostring): .log}) | add // {}) as $logs
    | {run_id:$run.id, run_attempt:$run.run_attempt, workflow:($run.name // "" | clean | .[0:200]),
       conclusion:($run.conclusion | clean), html_url:($run.html_url // "" | clean), stale:$stale,
       jobs_status:$jobs.status} + (if $jobs.status == "available" then {} else {jobs_reason:$jobs.reason} end)
    + {jobs:[$jobs.jobs[] | . + {log:$logs[.id | tostring]}], _cut:$jobs.cut}') || exit 1
  entries=$(printf '%s\n%s' "$entries" "$entry")
done
result=$(printf '%s\n' "$entries" | jq -sc --arg head "$head" --argjson cut "$truncated" \
  '{head_sha:$head, runs:map(del(._cut)), truncated:($cut or any(.[]; ._cut))}') || exit 1
[ "$(printf '%s' "$result" | wc -c)" -le 262144 ] || exit 1
printf '%s\n' "$result"
