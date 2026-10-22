use super::*;

/// Query IDs directly: gh pr view's rollup exporter drops both node IDs and database IDs.
pub(super) const PR_CHECKS_QUERY: &str = concat!(
    "query($owner:String!,$name:String!,$pr:Int!,$endCursor:String){",
    "repository(owner:$owner,name:$name){pullRequest(number:$pr){headRefOid mergeable mergeStateStatus ",
    "commits(last:1){nodes{commit{oid ",
    "checkSuites(first:100){totalCount nodes{id status} suitePageInfo:pageInfo{hasNextPage}} statusCheckRollup{",
    "contexts(first:100,after:$endCursor){totalCount nodes{__typename ",
    "... on CheckRun{id name status conclusion detailsUrl databaseId checkSuite{databaseId workflowRun{databaseId}}} ",
    "... on StatusContext{id context state targetUrl}} pageInfo{hasNextPage endCursor}}}}}}}}}",
);

/// Every page must still describe the first head, including the commit whose checks we read.
/// gh --paginate follows contexts.pageInfo; --slurp allows validation before the shared fold.
pub(super) const PR_CHECKS_PAGES_JQ: &str = include_str!("checks_snapshot.jq");

/// $1 PR, $2 repository selector, $3 fold, $4 query, $5 page normalization, $6 enrichment,
/// $7 sanitizer, $8 wait_for_all, $9 the after_rerun request or `null`. A fenced read reads the
/// rerun's run before the checks, so the checks are no older than the attempt it observed.
/// Recheck the current head after pagination; a moving read fails and the wait retries without
/// emitting evidence.
pub(super) const PR_CHECKS_READ_SCRIPT: &str = concat!(
    include_str!("actions_prelude.sh"),
    include_str!("checks_fence.sh"),
    "resolve_repo \"$2\" || exit 1\n",
    "read_fence \"$9\" || exit 1\n",
    "pages=$({ timeout --foreground 10 gh api graphql --hostname \"$host\" --paginate --slurp -F owner=\"${repo%/*}\" -F name=\"${repo##*/}\" ",
    "-F pr=\"$1\" -f query=\"$4\"; status=$?; printf '\\ngh-read-status:%s\\n' \"$status\"; } | head -c 2097216) || exit 1\n",
    "[ \"${pages##*\n}\" = gh-read-status:0 ] || exit 1\n",
    "pages=${pages%\ngh-read-status:0}\n",
    "[ \"$(printf '%s' \"$pages\" | wc -c)\" -le 2097152 ] || exit 1\n",
    "out=$(printf '%s\\n' \"$pages\" | jq -rc --argjson wait_for_all \"$8\" --argjson fence \"$fence\" \"$5$3)\") || exit 1\n",
    "head=${out%%\n*}\n",
    "body=${out#*\n}\n",
    "case \"$body\" in wait*|refuse*) prefix=${body%%\n*}; rest=${body#*\n}; ",
    "rest=$(printf '%s\\n' \"$rest\" | jq -c \"$7 .failed_checks |= map(clean_check + {diagnostics:{status:\\\"unavailable\\\",reason:\\\"diagnostics not collected while checks are pending\\\"}})\") || exit 1; ",
    "body=$(printf '%s\\n%s' \"$prefix\" \"$rest\");; *) original=$body; body=$(printf '%s\\n' \"$body\" | sh -c \"$6\" sh \"$host\" \"$repo\" \"$head\" \"$7\") || ",
    "{ prefix=; case \"$original\" in settle*) prefix=${original%%\n*}; original=${original#*\n};; esac; ",
    "rest=$(printf '%s\\n' \"$original\" | jq -c \"$7 .failed_checks |= map(clean_check + {diagnostics:{status:\\\"unavailable\\\",reason:\\\"diagnostic enrichment failed\\\"}})\") || exit 1; ",
    "body=$(if [ -n \"$prefix\" ]; then printf '%s\\n' \"$prefix\"; fi; printf '%s' \"$rest\"); };; esac\n",
    "current=$(timeout --foreground 3 gh pr view \"$1\" --repo \"$2\" --json headRefOid,mergeStateStatus) || exit 1\n",
    "state=$(printf '%s' \"$pages\" | jq -r '.[0].data.repository.pullRequest.mergeStateStatus') || exit 1\n",
    "printf '%s' \"$current\" | jq -e --arg head \"$head\" --arg state \"$state\" '.headRefOid == $head and .mergeStateStatus == $state' >/dev/null || exit 1\n",
    "[ \"$(printf '%s' \"$body\" | wc -c)\" -le 4194304 ] || exit 1\n",
    "printf '%s\\n' \"$body\"\n",
);

/// The checks enrichment, run after the shared Actions prelude that defines its `gh_api`.
pub(super) const PR_CHECKS_ENRICH_SCRIPT: &str = concat!(
    include_str!("actions_prelude.sh"),
    include_str!("checks_enrich.sh"),
);

/// Folds normalized GraphQL nodes into a verdict, snapshot and failed-check locators.
/// A CheckRun is pending until its
/// `status` is COMPLETED (an unfinished run exports `conclusion: ""`), then green only for
/// SUCCESS, NEUTRAL or SKIPPED; a StatusContext has only `state`, pending while PENDING or
/// EXPECTED. Any other finished value is a failure, which outranks pending: one failed check
/// means this head cannot go green. An empty or null rollup is `no_checks`.
/// `$fence` (the after_rerun request plus the rerun's observed run, or null) keeps every CheckRun
/// of the rerun's workflow run pending until that run reports a later attempt that completed, so
/// a read that still sees the rerun attempt's failure waits instead. It never makes a check
/// green, and checks of other runs fold as without it. A macro so the wait's program is built
/// from this one fold at compile time.
macro_rules! pr_checks_fold {
    () => {
        concat!(
            "[(.statusCheckRollup // [])[] | . as $check | ",
            "if .__typename == \"CheckRun\" then ",
            "(if $fence != null and .checkSuite.workflowRun.databaseId == $fence.run_id and ",
            "(($fence.observed_attempt > $fence.run_attempt and $fence.status == \"completed\") | not) ",
            "then \"pending\" ",
            "elif .status != \"COMPLETED\" then \"pending\" ",
            "elif .conclusion == \"SUCCESS\" or .conclusion == \"NEUTRAL\" or .conclusion == \"SKIPPED\" ",
            "then \"success\" else \"failure\" end) ",
            "elif .state == \"SUCCESS\" then \"success\" ",
            "elif .state == \"PENDING\" or .state == \"EXPECTED\" then \"pending\" ",
            "else \"failure\" end | {check: $check, verdict: .}] as $checks | ",
            ". as $pr | {conclusion: ($checks | map(.verdict) | ",
            "if any(. == \"failure\") then \"failure\" ",
            "elif any(. == \"pending\") then \"pending\" ",
            "elif length == 0 then \"no_checks\" ",
            "elif $pr.suites_completed != true or $pr.mergeStateStatus != \"CLEAN\" or $pr.mergeable != \"MERGEABLE\" then \"pending\" else \"success\" end), ",
            "mergeable: (.mergeable | ascii_downcase), head_sha: .headRefOid, ",
            "snapshot: {head_sha: .headRefOid, mergeable: (.mergeable | ascii_downcase), ",
            "all_checks_completed: (($checks | length > 0 and all(.verdict != \"pending\")) and $pr.suites_completed)}, ",
            "failed_checks: [$checks[] | select(.verdict == \"failure\") | .check | ",
            "{name: (.name // .context), _source: .} + ",
            "((.detailsUrl // .targetUrl) as $url | ",
            "if $url != null and $url != \"\" then {url: $url} ",
            "elif .id != null and .id != \"\" then {id: .id} ",
            "else error(\"Failed check has no locator\") end)]}"
        )
    };
}

/// The shared fold both programs below are built from.
#[cfg(test)]
pub(super) const PR_CHECKS_FOLD: &str = pr_checks_fold!();

/// The one-shot read's program: the fold's object, the only output of the deadline snapshot.
/// It refuses an after_rerun run that is not of the PR head.
pub(super) const PR_CHECKS_JQ: &str = concat!(
    "(",
    pr_checks_fold!(),
    ") | if $fence != null and $fence.head_sha != .head_sha ",
    "then error(\"after_rerun run is not of the PR head\") else . end",
);

/// The wait's program prints two lines: `<settle|wait|refuse> <head_sha>`, where `settle` means
/// the selected wait policy completes or the PR conflicts and `refuse` that the after_rerun run
/// is not of this head, then the fold's object as JSON.
/// jq prints string results raw, so the script reads the verdict without parsing JSON.
pub(super) const PR_CHECKS_WAIT_JQ: &str = concat!(
    "(",
    pr_checks_fold!(),
    ") as $r | \"\\(if $fence != null and $fence.head_sha != $r.head_sha then \"refuse\" ",
    "elif $r.conclusion == \"success\" or ",
    "($wait_for_all and $r.snapshot.all_checks_completed and $r.conclusion == \"failure\") or ",
    "($wait_for_all == false and $r.conclusion == \"failure\") ",
    "or $r.mergeable == \"conflicting\" then \"settle\" else \"wait\" end) ",
    "\\($r.head_sha // \"\")\", ($r | tojson)",
);

/// Seconds between the wait's complete, head-validated reads.
pub(super) const PR_CHECKS_POLL_SECS: u64 = 15;

/// `$1` PR, `$2` repo, `$3` poll seconds, `$4` [`PR_CHECKS_WAIT_JQ`]. Prints the JSON after the
/// verdict line of the first read that settles, or whose head is non-empty and differs from the
/// first non-empty head; a failed read is retried. A `refuse` of the first head exits 3: the
/// after_rerun run is not of the PR head. The parked deadline ends any other wait with the
/// output probe's snapshot.
pub(super) const PR_CHECKS_WAIT_SCRIPT: &str = concat!(
    "first=\n",
    "while :; do\n",
    "  if out=$(sh -c \"$5\" sh \"$1\" \"$2\" \"$4\" \"$6\" \"$7\" \"$8\" \"$9\" \"${10}\" \"${11}\"); then\n",
    "    read -r verdict head <<EOF\n",
    "$out\n",
    "EOF\n",
    "    [ -n \"$first\" ] || first=$head\n",
    "    if [ \"$verdict\" = settle ] || { [ -n \"$head\" ] && [ \"$head\" != \"$first\" ]; }; then\n",
    "      printf '%s\\n' \"${out#*\n}\"; exit 0\n",
    "    fi\n",
    "    if [ \"$verdict\" = refuse ]; then\n",
    "      echo 'after_rerun run is not of the PR head' >&2; exit 3\n",
    "    fi\n",
    "  fi\n",
    "  sleep \"$3\"\n",
    "done\n",
);

pub(super) fn lower_gh_pr_checks(args: &Value) -> Result<Value, String> {
    // Idempotent read: no mutating landed-verdict probe is attached.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let attempt = optional_attempt(args)?;
    let wait_for_all = match args.get("wait_for_all") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("wait_for_all must be a boolean".into()),
    };
    let after_rerun = optional_after_rerun(args)?;
    // `replay_any_version` replays by this key alone, so the key encodes every argument
    // injectively in a versioned namespace no released version wrote. A row stored under a
    // released key (`gh.pr.checks:{repo}:{pr}[:{attempt}]`, which is ambiguous, or
    // `gh_pr_checks_all:[…]`) is never replayed. A fenced wait has its own namespace, so it never
    // replays an unfenced read of the attempt it waits past. Completeness validation
    // uses new namespaces so results recorded before the stronger fence are never replayed.
    let idem_key = match after_rerun {
        Some((run_id, run_attempt)) => {
            let arguments =
                json!([repo, pr, run_id, run_attempt, wait_for_all, attempt]).to_string();
            format!("gh_pr_checks_rerun:v2:{arguments}")
        }
        None => {
            let arguments = json!([repo, pr, attempt]).to_string();
            if wait_for_all {
                format!("gh_pr_checks_all:v3:{arguments}")
            } else {
                format!("gh_pr_checks:v3:{arguments}")
            }
        }
    };
    let fence = match after_rerun {
        Some((run_id, run_attempt)) => {
            json!({ "run_id": run_id, "run_attempt": run_attempt }).to_string()
        }
        None => "null".to_string(),
    };
    let wait = vec![
        "sh".into(),
        "-c".into(),
        PR_CHECKS_WAIT_SCRIPT.into(),
        "sh".into(),
        pr.to_string(),
        repo.clone(),
        PR_CHECKS_POLL_SECS.to_string(),
        PR_CHECKS_WAIT_JQ.into(),
        PR_CHECKS_READ_SCRIPT.into(),
        PR_CHECKS_QUERY.into(),
        PR_CHECKS_PAGES_JQ.into(),
        PR_CHECKS_ENRICH_SCRIPT.into(),
        checks_sanitizer(),
        wait_for_all.to_string(),
        fence.clone(),
    ];
    // The deadline snapshot reads once; it is never the waiting script.
    let read = vec![
        "sh".to_string(),
        "-c".into(),
        PR_CHECKS_READ_SCRIPT.into(),
        "sh".into(),
        pr.to_string(),
        repo.clone(),
        PR_CHECKS_JQ.into(),
        PR_CHECKS_QUERY.into(),
        PR_CHECKS_PAGES_JQ.into(),
        PR_CHECKS_ENRICH_SCRIPT.into(),
        checks_sanitizer(),
        wait_for_all.to_string(),
        fence,
    ];
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    let mut payload = forge_payload(
        wait,
        idem_key,
        Some(event_spec(
            "forge.pr.checks",
            [
                ("conclusion", json_field("/conclusion")),
                ("mergeable", json_field("/mergeable")),
                ("head_sha", json_field("/head_sha")),
                ("snapshot", json_field("/snapshot")),
                ("failed_checks", json_field("/failed_checks")),
            ],
        )),
        json!({ "pr_number": pr }),
        Some(json!({
            "probe_argv": [
                "gh",
                "pr",
                "view",
                pr.to_string(),
                "--repo",
                repo.clone(),
                "--json",
                "state"
            ],
            "output_probe_argv": read
        })),
        true,
    )?;
    // An idempotent read: a same-key retry spanning a program change replays the stored operation.
    payload["replay_any_version"] = json!(true);
    Ok(payload)
}

/// `after_rerun`: the `{run_id, run_attempt}` of the run attempt that failed and was rerun, a
/// closed object of positive integers; absent or null for an unfenced wait.
fn optional_after_rerun(args: &Value) -> Result<Option<(u64, u64)>, String> {
    let value = match args.get("after_rerun") {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let object = value
        .as_object()
        .ok_or_else(|| "after_rerun must be an object {run_id, run_attempt}".to_string())?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "run_id" | "run_attempt"))
    {
        return Err(format!(
            "after_rerun: unknown field `{key}`; valid: run_attempt, run_id"
        ));
    }
    let positive = |key: &str| match object.get(key).and_then(Value::as_u64) {
        Some(value) if value > 0 => Ok(value),
        _ => Err(format!("after_rerun.{key} must be a positive integer")),
    };
    Ok(Some((positive("run_id")?, positive("run_attempt")?)))
}

/// Derive exact-value redaction from the same authority as credential passthrough.
pub(super) fn checks_sanitizer() -> String {
    format!(
        "def credential_keys: {};\n{}",
        serde_json::to_string(calm_types::forge_env::FORGE_CREDENTIAL_ENV_KEYS).unwrap(),
        include_str!("checks_sanitize.jq")
    )
}
