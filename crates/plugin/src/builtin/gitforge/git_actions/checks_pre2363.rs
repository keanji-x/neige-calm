//! Frozen pre-2363 semantic descriptor: identity only, never execution.
use super::*;

/// Query IDs directly: gh pr view's rollup exporter drops both node IDs and database IDs.
const PR_CHECKS_QUERY: &str = concat!(
    "query($owner:String!,$name:String!,$pr:Int!,$endCursor:String){",
    "repository(owner:$owner,name:$name){pullRequest(number:$pr){headRefOid mergeable ",
    "commits(last:1){nodes{commit{oid statusCheckRollup{",
    "contexts(first:100,after:$endCursor){nodes{__typename ",
    "... on CheckRun{id name status conclusion detailsUrl} ",
    "... on StatusContext{id context state targetUrl}} pageInfo{hasNextPage endCursor}}}}}}}}}",
);

/// Every page must still describe the first head, including the commit whose checks we read.
/// gh --paginate follows contexts.pageInfo; --slurp allows validation before the shared fold.
const PR_CHECKS_PAGES_JQ: &str = concat!(
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
const PR_CHECKS_READ_SCRIPT: &str = concat!(
    "identity=$(gh repo view \"$2\" --json nameWithOwner,url --jq '(.url | split(\"/\")[2]), .nameWithOwner') || exit 1\n",
    "host=${identity%%\n*}\nrepo=${identity#*\n}\n",
    "pages=$(gh api graphql --hostname \"$host\" --paginate --slurp -F owner=\"${repo%/*}\" -F name=\"${repo##*/}\" ",
    "-F pr=\"$1\" -f query=\"$4\") || exit 1\n",
    "out=$(printf '%s\\n' \"$pages\" | jq -rc \"$5$3)\") || exit 1\n",
    "head=${out%%\n*}\n",
    "current=$(gh pr view \"$1\" --repo \"$2\" --json headRefOid --jq .headRefOid) || exit 1\n",
    "[ \"$head\" = \"$current\" ] || exit 1\n",
    "printf '%s\\n' \"${out#*\n}\"\n",
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
            "snapshot: {head_sha: .headRefOid, mergeable: (.mergeable | ascii_downcase)}, ",
            "failed_checks: [$checks[] | select(.verdict == \"failure\") | .check | ",
            "{name: (.name // .context)} + ",
            "((.detailsUrl // .targetUrl) as $url | ",
            "if $url != null and $url != \"\" then {url: $url} ",
            "elif .id != null and .id != \"\" then {id: .id} ",
            "else error(\"Failed check has no locator\") end)]}"
        )
    };
}

/// The one-shot read's program: the fold's object, the only output of the deadline snapshot.
const PR_CHECKS_JQ: &str = pr_checks_fold!();

pub(super) fn hash(payload: &Value, repo: &str, pr: u64) -> Result<String, String> {
    let mut legacy: calm_types::forge_action::PluginForgePayload =
        serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
    // Freeze every semantic field except the parameterized old identity. Future extractors
    // or context fields must not retroactively alter the released predecessor's hash.
    let field = |path: &str| FieldSource::JsonField { path: path.into() };
    legacy.event_spec = Some(event_spec(
        "forge.pr.checks",
        [
            ("conclusion", field("/conclusion")),
            ("mergeable", field("/mergeable")),
            ("head_sha", field("/head_sha")),
            ("snapshot", field("/snapshot")),
            ("failed_checks", field("/failed_checks")),
        ],
    ));
    legacy.subject = None;
    legacy.context = [("pr_number".to_string(), json!(pr))].into_iter().collect();
    let probe = legacy.probe.as_mut().expect("checks probe");
    probe.probe_argv = vec![
        "gh".into(),
        "pr".into(),
        "view".into(),
        pr.to_string(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
        "state".into(),
    ];
    probe.output_probe_argv = Some(vec![
        "sh".into(),
        "-c".into(),
        PR_CHECKS_READ_SCRIPT.into(),
        "sh".into(),
        pr.to_string(),
        repo.into(),
        PR_CHECKS_JQ.into(),
        PR_CHECKS_QUERY.into(),
        PR_CHECKS_PAGES_JQ.into(),
    ]);
    calm_types::forge_action::semantic_payload_hash(&legacy).map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::forge_action::{PluginForgePayload, semantic_payload_hash};

    #[test]
    fn gh_pr_checks_pre2363_freezes_the_complete_semantic_descriptor() {
        let frozen: PluginForgePayload =
            serde_json::from_str(include_str!("checks-pre-2363.json")).unwrap();
        let expected = semantic_payload_hash(&frozen).unwrap();
        let mut payload = lower(
            "gh_pr_checks",
            &json!({"repo":"owner/repo","pr":42,"attempt":"upgrade"}),
        )
        .unwrap();
        assert_eq!(payload["compatible_payload_hashes"][1], expected);
        payload["event_spec"]["fields"]["future_field"] =
            json!({"json_field":{"path":"/future_field"}});
        payload["context"]["future_field"] = json!("new semantics");
        payload["probe"]["probe_argv"] = json!(["future-probe"]);
        assert_eq!(hash(&payload, "owner/repo", 42).unwrap(), expected);
    }
}
