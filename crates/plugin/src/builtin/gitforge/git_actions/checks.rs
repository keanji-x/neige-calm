use super::*;

/// Query IDs directly: gh pr view's rollup exporter drops both node IDs and database IDs.
pub(super) const PR_CHECKS_QUERY: &str = concat!(
    "query($owner:String!,$name:String!,$pr:Int!,$endCursor:String){",
    "repository(owner:$owner,name:$name){pullRequest(number:$pr){headRefOid mergeable ",
    "commits(last:1){nodes{commit{oid statusCheckRollup{",
    "contexts(first:100,after:$endCursor){nodes{__typename ",
    "... on CheckRun{id name status conclusion detailsUrl databaseId checkSuite{databaseId workflowRun{databaseId}}} ",
    "... on StatusContext{id context state targetUrl}} pageInfo{hasNextPage endCursor}}}}}}}}}",
);

/// Every page must still describe the first head, including the commit whose checks we read.
/// gh --paginate follows contexts.pageInfo; --slurp allows validation before the shared fold.
pub(super) const PR_CHECKS_PAGES_JQ: &str = concat!(
    "map(.data.repository.pullRequest) as $pages | $pages[0] as $first | ",
    "if ($first.headRefOid == null or $first.headRefOid == \"\") or ",
    "any($pages[]; .headRefOid != $first.headRefOid or ",
    ".commits.nodes[0].commit.oid != $first.headRefOid) then error(\"PR head moved during checks pagination\") else ",
    "{headRefOid: $first.headRefOid, mergeable: $first.mergeable, ",
    "statusCheckRollup: [$pages[].commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]?]} end | ",
    ".headRefOid, (",
);

/// $1 PR, $2 repository selector, $3 fold, $4 query, $5 page normalization. Recheck the current
/// head after pagination; a moving read fails and the wait retries without emitting evidence.
pub(super) const PR_CHECKS_READ_SCRIPT: &str = concat!(
    "identity=$(timeout --foreground 3 gh repo view \"$2\" --json nameWithOwner,url --jq '(.url | split(\"/\")[2]), .nameWithOwner') || exit 1\n",
    "host=${identity%%\n*}\nrepo=${identity#*\n}\n",
    "pages=$({ timeout --foreground 10 gh api graphql --hostname \"$host\" --paginate --slurp -F owner=\"${repo%/*}\" -F name=\"${repo##*/}\" ",
    "-F pr=\"$1\" -f query=\"$4\"; status=$?; printf '\\ngh-read-status:%s\\n' \"$status\"; } | head -c 2097216) || exit 1\n",
    "[ \"${pages##*\n}\" = gh-read-status:0 ] || exit 1\n",
    "pages=${pages%\ngh-read-status:0}\n",
    "[ \"$(printf '%s' \"$pages\" | wc -c)\" -le 2097152 ] || exit 1\n",
    "out=$(printf '%s\\n' \"$pages\" | jq -rc --argjson wait_for_all \"$8\" \"$5$3)\") || exit 1\n",
    "head=${out%%\n*}\n",
    "body=${out#*\n}\n",
    "case \"$body\" in wait*) prefix=${body%%\n*}; rest=${body#*\n}; ",
    "rest=$(printf '%s\\n' \"$rest\" | jq -c \"$7 .failed_checks |= map(clean_check + {diagnostics:{status:\\\"unavailable\\\",reason:\\\"diagnostics not collected while checks are pending\\\"}})\") || exit 1; ",
    "body=$(printf '%s\\n%s' \"$prefix\" \"$rest\");; *) original=$body; body=$(printf '%s\\n' \"$body\" | sh -c \"$6\" sh \"$host\" \"$repo\" \"$head\" \"$7\") || ",
    "{ prefix=; case \"$original\" in settle*) prefix=${original%%\n*}; original=${original#*\n};; esac; ",
    "rest=$(printf '%s\\n' \"$original\" | jq -c \"$7 .failed_checks |= map(clean_check + {diagnostics:{status:\\\"unavailable\\\",reason:\\\"diagnostic budget exceeded or API unavailable\\\"}})\") || exit 1; ",
    "body=$(if [ -n \"$prefix\" ]; then printf '%s\\n' \"$prefix\"; fi; printf '%s' \"$rest\"); };; esac\n",
    "current=$(timeout --foreground 3 gh pr view \"$1\" --repo \"$2\" --json headRefOid --jq .headRefOid) || exit 1\n",
    "[ \"$head\" = \"$current\" ] || exit 1\n",
    "[ \"$(printf '%s' \"$body\" | wc -c)\" -le 4194304 ] || exit 1\n",
    "printf '%s\\n' \"$body\"\n",
);

/// Folds normalized GraphQL nodes into a verdict, snapshot and failed-check locators.
/// A CheckRun is pending until its
/// `status` is COMPLETED (an unfinished run exports `conclusion: ""`), then green only for
/// SUCCESS, NEUTRAL or SKIPPED; a StatusContext has only `state`, pending while PENDING or
/// EXPECTED. Any other finished value is a failure, which outranks pending: one failed check
/// means this head cannot go green. An empty or null rollup is `no_checks`. A macro so the wait's
/// program is built from this one fold at compile time.
macro_rules! pr_checks_fold {
    () => {
        concat!(
            "[(.statusCheckRollup // [])[] | . as $check | ",
            "if .__typename == \"CheckRun\" then ",
            "(if .status != \"COMPLETED\" then \"pending\" ",
            "elif .conclusion == \"SUCCESS\" or .conclusion == \"NEUTRAL\" or .conclusion == \"SKIPPED\" ",
            "then \"success\" else \"failure\" end) ",
            "elif .state == \"SUCCESS\" then \"success\" ",
            "elif .state == \"PENDING\" or .state == \"EXPECTED\" then \"pending\" ",
            "else \"failure\" end | {check: $check, verdict: .}] as $checks | ",
            "{conclusion: ($checks | map(.verdict) | ",
            "if any(. == \"failure\") then \"failure\" ",
            "elif any(. == \"pending\") then \"pending\" ",
            "elif length == 0 then \"no_checks\" ",
            "else \"success\" end), ",
            "mergeable: (.mergeable | ascii_downcase), head_sha: .headRefOid, ",
            "snapshot: {head_sha: .headRefOid, mergeable: (.mergeable | ascii_downcase), ",
            "all_checks_completed: ($checks | length > 0 and all(.verdict != \"pending\"))}, ",
            "failed_checks: [$checks[] | select(.verdict == \"failure\") | .check | ",
            "{name: (.name // .context), _source: .} + ",
            "((.detailsUrl // .targetUrl) as $url | ",
            "if $url != null and $url != \"\" then {url: $url} ",
            "elif .id != null and .id != \"\" then {id: .id} ",
            "else error(\"Failed check has no locator\") end)]}"
        )
    };
}

/// The one-shot read's program: the fold's object, the only output of the deadline snapshot.
pub(super) const PR_CHECKS_JQ: &str = pr_checks_fold!();

/// The wait's program prints two lines: `<settle|wait> <head_sha>`, where `settle` means the
/// the selected wait policy completes or the PR conflicts, then the fold's object as JSON.
/// jq prints string results raw, so the script reads the verdict without parsing JSON.
pub(super) const PR_CHECKS_WAIT_JQ: &str = concat!(
    "(",
    pr_checks_fold!(),
    ") as $r | \"\\(if $r.conclusion == \"success\" or ",
    "($wait_for_all and $r.snapshot.all_checks_completed) or ",
    "($wait_for_all == false and $r.conclusion == \"failure\") ",
    "or $r.mergeable == \"conflicting\" then \"settle\" else \"wait\" end) ",
    "\\($r.head_sha // \"\")\", ($r | tojson)",
);

/// Seconds between the wait's complete, head-validated reads.
pub(super) const PR_CHECKS_POLL_SECS: u64 = 15;

/// `$1` PR, `$2` repo, `$3` poll seconds, `$4` [`PR_CHECKS_WAIT_JQ`]. Prints the JSON after the
/// verdict line of the first read that settles, or whose head is non-empty and differs from the
/// first non-empty head; a failed read is retried. The parked deadline ends any other wait with
/// the output probe's snapshot.
pub(super) const PR_CHECKS_WAIT_SCRIPT: &str = concat!(
    "first=\n",
    "while :; do\n",
    "  if out=$(sh -c \"$5\" sh \"$1\" \"$2\" \"$4\" \"$6\" \"$7\" \"$8\" \"$9\" \"${10}\"); then\n",
    "    read -r verdict head <<EOF\n",
    "$out\n",
    "EOF\n",
    "    [ -n \"$first\" ] || first=$head\n",
    "    if [ \"$verdict\" = settle ] || { [ -n \"$head\" ] && [ \"$head\" != \"$first\" ]; }; then\n",
    "      printf '%s\\n' \"${out#*\n}\"; exit 0\n",
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
    let mut idem_key = match attempt.as_deref() {
        Some(attempt) => format!("gh.pr.checks:{repo}:{pr}:{attempt}"),
        None => format!("gh.pr.checks:{repo}:{pr}"),
    };
    if wait_for_all {
        idem_key = format!(
            "gh_pr_checks_all:{}",
            serde_json::to_string(&json!([repo, pr, attempt])).unwrap()
        );
    }
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
        include_str!("checks_enrich.sh").into(),
        checks_sanitizer(),
        wait_for_all.to_string(),
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
        include_str!("checks_enrich.sh").into(),
        checks_sanitizer(),
        wait_for_all.to_string(),
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
    if !wait_for_all {
        payload["compatible_payload_hashes"] = json!([
            checks_compat::predecessor_hash(&payload, &repo, pr)?,
            checks_pre2363::hash(&payload, &repo, pr)?,
        ]);
    }
    Ok(payload)
}

/// Derive exact-value redaction from the same authority as credential passthrough.
pub(super) fn checks_sanitizer() -> String {
    format!(
        "def credential_keys: {};\n{}",
        serde_json::to_string(calm_types::forge_env::FORGE_CREDENTIAL_ENV_KEYS).unwrap(),
        include_str!("checks_sanitize.jq")
    )
}
