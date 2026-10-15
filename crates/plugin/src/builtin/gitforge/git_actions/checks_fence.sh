# The after_rerun fence of the checks read; runs after the shared Actions prelude and
# resolve_repo. $1 the request {run_id, run_attempt}, or null for an unfenced read.
# Sets fence to null, or to the request plus the run's observed head_sha, run attempt and
# status, which the shared fold reads as $fence.
read_fence() {
  fence=null
  [ "$1" != null ] || return 0
  budget_end=$(($(date +%s) + 10))
  run_id=$(printf '%s\n' "$1" | jq -er '.run_id | numbers') || return 1
  run=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$run_id" --jq '{id,head_sha,run_attempt,status}') || return 1
  fence=$(printf '%s\n' "$run" | jq -ec --argjson want "$1" '
    select(.id == $want.run_id and (.head_sha | type == "string") and (.run_attempt | type == "number")
      and (.status | type == "string"))
    | $want + {head_sha, observed_attempt: .run_attempt, status}') || return 1
}
