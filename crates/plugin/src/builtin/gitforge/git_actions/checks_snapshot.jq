# Normalize only a complete, consistent snapshot of this head. A changing count,
# duplicated node, API error, or truncated connection is retried by the wait.
if type != "array" or length == 0 or any(.[]; .errors != null) then
  error("checks API returned incomplete evidence")
else . end |
map(.data.repository.pullRequest) as $pages | $pages[0] as $first |
$first.commits.nodes[0].commit as $commit |
[$pages[].commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]?] as $nodes |
($commit.statusCheckRollup.contexts.totalCount //
  (if $commit.statusCheckRollup == null then 0 else null end)) as $total |
if ($first.headRefOid | type != "string" or length == 0) or
  any($pages[]; .headRefOid != $first.headRefOid or
    .commits.nodes[0].commit.oid != $first.headRefOid or
    .mergeable != $first.mergeable or .mergeStateStatus != $first.mergeStateStatus or
    .commits.nodes[0].commit.checkSuites != $commit.checkSuites or
    .commits.nodes[0].commit.statusCheckRollup.contexts.totalCount !=
      $commit.statusCheckRollup.contexts.totalCount) then
  error("PR checks snapshot changed during pagination")
elif ($total | type != "number" or . < 0) or ($nodes | length) != $total or
  ($nodes | map(.id) | unique | length) != $total or
  any($nodes[]; .id | type != "string" or length == 0) or
  any($pages[]; .commits.nodes[0].commit.statusCheckRollup as $rollup |
    $rollup != null and ($rollup.contexts.pageInfo.hasNextPage | type != "boolean")) or
  any(range(0; $pages|length); . as $i |
    ($pages[$i].commits.nodes[0].commit.statusCheckRollup.contexts.pageInfo.hasNextPage // false)
      != ($i < ($pages|length) - 1)) then
  error("PR checks context snapshot incomplete")
elif ($commit.checkSuites.totalCount | type != "number" or . < 0) or
  ($commit.checkSuites.nodes | type != "array") or
  (any($nodes[]; .__typename == "CheckRun") and $commit.checkSuites.totalCount == 0) or
  ($commit.checkSuites.nodes | length) != $commit.checkSuites.totalCount or
  ($commit.checkSuites.nodes | map(.id) | unique | length) != $commit.checkSuites.totalCount or
  any($commit.checkSuites.nodes[]; .id | type != "string" or length == 0) or
  $commit.checkSuites.suitePageInfo.hasNextPage != false then
  error("PR check-suite snapshot incomplete (maximum 100 suites)")
else
  {headRefOid: $first.headRefOid, mergeable: $first.mergeable,
    mergeStateStatus: $first.mergeStateStatus, statusCheckRollup: $nodes,
    suites_completed: all($commit.checkSuites.nodes[]; .status == "COMPLETED")}
end | .headRefOid, (
