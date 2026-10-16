# Re-run the failed jobs of one completed workflow run attempt of a PR's head (#2617), once the
# user approved exactly this request. Runs after the shared Actions prelude. $1 repository
# selector, $2 PR number, $3 head SHA, $4 run id, $5 run attempt.
# Nothing is posted unless the PR's head is $3 (else exit 20) and the run is $4 of that head and
# repository, completed, at attempt $5 (else exit 21); a read that fails exits 22. Then the one
# POST: its failure exits with gh's status, and whether GitHub took the request is unknown.
budget_end=$(($(date +%s) + 60))
resolve_repo "$1" || exit 22
case "$host" in ''|*[!a-zA-Z0-9.-]*) exit 22 ;; esac
pr_head=$(timeout --foreground 20 gh pr view "$2" --repo "$1" --json headRefOid --jq .headRefOid) || exit 22
[ "$pr_head" = "$3" ] || exit 20
run=$(gh_api --hostname "$host" "repos/$repo/actions/runs/$4" \
  --jq '{id, head_sha, run_attempt, status, full_name: .repository.full_name}') || exit 22
printf '%s\n' "$run" | jq -e --argjson id "$4" --arg head "$3" --arg repo "$repo" --argjson attempt "$5" \
  '.id == $id and .head_sha == $head and .full_name == $repo and .status == "completed"
   and .run_attempt == $attempt' >/dev/null || exit 21
gh_api --hostname "$host" -X POST "repos/$repo/actions/runs/$4/rerun-failed-jobs" >/dev/null
