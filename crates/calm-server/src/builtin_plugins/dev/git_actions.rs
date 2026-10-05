use std::collections::BTreeMap;

use calm_types::event::{FieldSource, ForgeEventSpec};
use calm_types::forge_git::{
    FORGE_SHELL_PRELUDE, GIT_COMMIT_OUTPUT_PROBE_SCRIPT, GIT_COMMIT_PROBE_SCRIPT, GIT_COMMIT_SCRIPT,
};
use serde_json::{Value, json};

use crate::plugin_host::forge_caller::ForgeCallerScope;

mod issue;

pub fn lower(tool: &str, args: &Value) -> Result<Value, String> {
    match tool {
        "git.worktree.add" => lower_git_worktree_add(args),
        "git.commit" => lower_git_commit(args),
        "gh.pr.list" => lower_gh_pr_list(args),
        "gh.pr.diff" => lower_gh_pr_diff(args),
        "gh.pr.checks" => lower_gh_pr_checks(args),
        "gh.pr.merge" => lower_gh_pr_merge(args),
        "gh.issue.view" => lower_gh_issue_view(args),
        "gh.issue.close" => lower_gh_issue_close(args),
        "gh.issue.comment" => Err("issue comments require trusted forge caller metadata".into()),
        "gh.issue.comments" => issue::comments(args),
        _ => Err(format!("unknown git-forge tool `{tool}`")),
    }
}

/// Caller-sensitive lowering stays in the owning plugin; the kernel only provides identity.
pub fn lower_for_caller(
    tool: &str,
    args: &Value,
    caller: &ForgeCallerScope,
) -> Result<Value, String> {
    caller.validate()?;
    if caller.plugin_id != super::PLUGIN_ID {
        return Err("forge caller plugin does not match development plugin".into());
    }
    match tool {
        "gh.issue.comment" => issue::comment(args, caller),
        _ => lower(tool, args),
    }
}

fn lower_git_worktree_add(args: &Value) -> Result<Value, String> {
    let target = required_string(args, "target")?;
    let branch = optional_string(args, "branch")?;
    // `git worktree add` checks out, so it runs the repository's hooks and filters (#1830 S3 D4).
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("{FORGE_SHELL_PRELUDE}\nneige_git worktree add \"$@\""),
        "sh".to_string(),
        target.clone(),
    ];
    if let Some(branch) = branch {
        argv.push("-b".into());
        argv.push(branch);
    }
    forge_payload(
        argv,
        format!("git.worktree.add:{target}"),
        Some(event_spec("worktree.provisioned", [])),
        json!({ "path": target }),
        None,
        false,
    )
}

fn lower_git_commit(args: &Value) -> Result<Value, String> {
    let message = required_string(args, "message")?;
    let idem = required_string(args, "idem")?;
    let branch = optional_string(args, "branch")?;
    let mut argv = vec![
        "sh".into(),
        "-c".into(),
        git_commit_script(),
        "sh".into(),
        message,
    ];
    if let Some(branch) = branch.as_ref() {
        argv.push(branch.clone());
    }
    let mut output_probe_argv = vec![
        "sh".into(),
        "-c".into(),
        git_commit_output_probe_script(),
        "sh".into(),
    ];
    if let Some(branch) = branch {
        output_probe_argv.push(branch);
    }
    forge_payload(
        argv,
        format!("git.commit:{idem}"),
        Some(event_spec(
            "worktree.committed",
            [
                (
                    "commit_sha",
                    FieldSource::JsonField {
                        path: "/commit".into(),
                    },
                ),
                (
                    "branch",
                    FieldSource::JsonField {
                        path: "/branch".into(),
                    },
                ),
            ],
        )),
        json!({}),
        Some(json!({
            // Idempotent contract: after a nonzero `git commit`, an empty index
            // means the requested commit landed already or there was nothing to commit.
            "probe_argv": [
                "sh",
                "-c",
                git_commit_probe_script(),
                "sh"
            ],
            "output_probe_argv": output_probe_argv
        })),
        false,
    )
}

/// The Planner commit's `sh -c` text: the credential split, then the shared script.
fn git_commit_script() -> String {
    format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_SCRIPT}")
}

/// Its probe's: `git status` runs the repository's fsmonitor and filters, so it gets the split too.
fn git_commit_probe_script() -> String {
    format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_PROBE_SCRIPT}")
}

/// Its output probe's: `git log` can run the repository's `gpg.program`, so it gets the split too.
fn git_commit_output_probe_script() -> String {
    format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_OUTPUT_PROBE_SCRIPT}")
}

fn lower_gh_pr_list(args: &Value) -> Result<Value, String> {
    // Idempotent read: no mutating landed-verdict probe is attached.
    let repo = required_string(args, "repo")?;
    let base = required_string(args, "base")?;
    let head = required_string(args, "head")?;
    let argv = vec![
        "gh".into(),
        "pr".into(),
        "list".into(),
        "--repo".into(),
        repo.clone(),
        "--base".into(),
        base.clone(),
        "--head".into(),
        head.clone(),
        "--state".into(),
        "open".into(),
        "--json".into(),
        "number".into(),
        "--jq".into(),
        "[.[].number]".into(),
    ];
    forge_payload(
        argv.clone(),
        format!("gh.pr.list:{repo}:{base}:{head}"),
        Some(event_spec(
            "forge.scan.completed",
            [(
                "overlapping_prs",
                FieldSource::JsonField {
                    path: String::new(),
                },
            )],
        )),
        json!({}),
        Some(json!({
            "probe_argv": [
                "gh",
                "pr",
                "list",
                "--repo",
                repo,
                "--limit",
                "1"
            ],
            "output_probe_argv": argv
        })),
        true,
    )
}

fn lower_gh_pr_diff(args: &Value) -> Result<Value, String> {
    // Idempotent read: intentionally probe-free.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let base_sha = required_string(args, "base_sha")?;
    let head_sha = required_string(args, "head_sha")?;
    forge_payload(
        vec![
            "gh".into(),
            "pr".into(),
            "diff".into(),
            pr.to_string(),
            "--repo".into(),
            repo.clone(),
            "--patch".into(),
        ],
        format!("gh.pr.diff:{repo}:{pr}:{base_sha}:{head_sha}"),
        Some(event_spec("forge.pr.diff.read", [])),
        json!({
            "pr_number": pr,
            "base_sha": base_sha,
            "head_sha": head_sha
        }),
        None,
        false,
    )
}

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

/// The wait's program prints two lines: `<settle|wait> <head_sha>`, where `settle` means the
/// checks concluded success or failure or the PR conflicts, then the fold's object as JSON.
/// jq prints string results raw, so the script reads the verdict without parsing JSON.
const PR_CHECKS_WAIT_JQ: &str = concat!(
    "(",
    pr_checks_fold!(),
    ") as $r | \"\\(if $r.conclusion == \"success\" or $r.conclusion == \"failure\" ",
    "or $r.mergeable == \"conflicting\" then \"settle\" else \"wait\" end) ",
    "\\($r.head_sha // \"\")\", ($r | tojson)",
);

/// Seconds between the wait's complete, head-validated reads.
const PR_CHECKS_POLL_SECS: u64 = 15;

/// `$1` PR, `$2` repo, `$3` poll seconds, `$4` [`PR_CHECKS_WAIT_JQ`]. Prints the JSON after the
/// verdict line of the first read that settles, or whose head is non-empty and differs from the
/// first non-empty head; a failed read is retried. The parked deadline ends any other wait with
/// the output probe's snapshot.
const PR_CHECKS_WAIT_SCRIPT: &str = concat!(
    "first=\n",
    "while :; do\n",
    "  if out=$(sh -c \"$5\" sh \"$1\" \"$2\" \"$4\" \"$6\" \"$7\"); then\n",
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

fn lower_gh_pr_checks(args: &Value) -> Result<Value, String> {
    // Idempotent read: no mutating landed-verdict probe is attached.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let attempt = optional_attempt(args)?;
    let idem_key = match attempt {
        Some(attempt) => format!("gh.pr.checks:{repo}:{pr}:{attempt}"),
        None => format!("gh.pr.checks:{repo}:{pr}"),
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
    ];
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    forge_payload(
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
                repo,
                "--json",
                "state"
            ],
            "output_probe_argv": read
        })),
        true,
    )
}

fn lower_gh_pr_merge(args: &Value) -> Result<Value, String> {
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let expected_head_sha = optional_string(args, "expected_head_sha")?;
    let mut argv = vec![
        "gh".into(),
        "pr".into(),
        "merge".into(),
        pr.to_string(),
        "--repo".into(),
        repo.clone(),
        "--squash".into(),
        "--delete-branch".into(),
    ];
    let idem_key = if let Some(expected_head_sha) = expected_head_sha.as_deref() {
        argv.push("--match-head-commit".into());
        argv.push(expected_head_sha.to_string());
        format!("gh.pr.merge:{repo}:{pr}:{expected_head_sha}")
    } else {
        format!("gh.pr.merge:{repo}:{pr}")
    };
    let probe_argv = if let Some(expected_head_sha) = expected_head_sha.as_deref() {
        json!([
            "sh",
            "-c",
            PR_MERGE_HEAD_MATCH_PROBE_SCRIPT,
            "sh",
            pr.to_string(),
            repo,
            expected_head_sha
        ])
    } else {
        json!([
            "sh",
            "-c",
            PR_MERGE_PROBE_SCRIPT,
            "sh",
            pr.to_string(),
            repo
        ])
    };
    let mut payload = forge_payload(
        argv,
        idem_key,
        Some(event_spec(
            "forge.pr.merged",
            [
                (
                    "head_sha",
                    FieldSource::JsonField {
                        path: "/headRefOid".into(),
                    },
                ),
                (
                    "merge_sha",
                    FieldSource::JsonField {
                        path: "/mergeCommit/oid".into(),
                    },
                ),
            ],
        )),
        json!({}),
        Some(json!({
            "probe_argv": probe_argv,
            "output_probe_argv": [
                "gh",
                "pr",
                "view",
                pr.to_string(),
                "--repo",
                repo,
                "--json",
                "headRefOid,mergeCommit"
            ]
        })),
        true,
    )?;
    payload["subject"] = json!({ "pr_number": pr });
    Ok(payload)
}

fn lower_gh_issue_view(args: &Value) -> Result<Value, String> {
    // Idempotent read: intentionally probe-free.
    let repo = required_string(args, "repo")?;
    let issue = required_u64(args, "issue")?;
    let idem_key = match optional_attempt(args)? {
        Some(attempt) => format!(
            "gh.issue.view:v3:{}",
            serde_json::to_string(&json!([repo, issue, attempt]))
                .map_err(|e| format!("encode issue read identity: {e}"))?
        ),
        None => format!("gh.issue.view:v2:{repo}:{issue}"),
    };
    issue::read_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "view".into(),
            issue.to_string(),
            "--repo".into(),
            repo.clone(),
            "--json".into(),
            "body".into(),
            "--jq".into(),
            ".body".into(),
        ],
        idem_key,
        issue,
    )
}

const ISSUE_CLOSE_PROBE_SCRIPT: &str = "out=$(gh issue view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; \
     case \"$out\" in *'\"state\":\"CLOSED\"'*) exit 0 ;; *) exit 1 ;; esac";
const PR_MERGE_PROBE_SCRIPT: &str = "out=$(gh pr view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; \
     case \"$out\" in *'\"state\":\"MERGED\"'*) exit 0 ;; *) exit 1 ;; esac";
const PR_MERGE_HEAD_MATCH_PROBE_SCRIPT: &str = "out=$(gh pr view \"$1\" --repo \"$2\" --json state,headRefOid 2>/dev/null) || exit 3; \
     case \"$out\" in *'\"state\":\"MERGED\"'*) case \"$out\" in *'\"headRefOid\":\"'\"$3\"'\"'*) exit 0 ;; *) exit 1 ;; esac ;; *) exit 1 ;; esac";
fn lower_gh_issue_close(args: &Value) -> Result<Value, String> {
    let repo = required_string(args, "repo")?;
    let issue = required_u64(args, "issue")?;
    forge_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "close".into(),
            issue.to_string(),
            "--repo".into(),
            repo.clone(),
        ],
        format!("gh.issue.close:{repo}:{issue}"),
        Some(event_spec("forge.issue.closed", [])),
        json!({ "issue_number": issue }),
        Some(json!({
            // Verdict-only recovery: CLOSED => 0/Landed, open => 1/NotLanded,
            // and gh invocation failure => 3/Unknown so infra outages stay retryable.
            "probe_argv": [
                "sh",
                "-c",
                ISSUE_CLOSE_PROBE_SCRIPT,
                "sh",
                issue.to_string(),
                repo
            ]
        })),
        true,
    )
}

fn forge_payload(
    argv: Vec<String>,
    idem_key: String,
    event_spec: Option<ForgeEventSpec>,
    context: Value,
    probe: Option<Value>,
    parked: bool,
) -> Result<Value, String> {
    let event_spec = match event_spec {
        Some(event_spec) => serde_json::to_value(event_spec)
            .map_err(|e| format!("serialize forge event spec: {e}"))?,
        None => Value::Null,
    };
    Ok(json!({
        "argv": argv,
        "idem_key": idem_key,
        "event_spec": event_spec,
        "subject": Value::Null,
        "context": context,
        "probe": probe.unwrap_or(Value::Null),
        "parked": parked
    }))
}

fn event_spec<const N: usize>(
    event_kind: &str,
    fields: [(&str, FieldSource); N],
) -> ForgeEventSpec {
    ForgeEventSpec {
        event_kind: event_kind.into(),
        fields: fields
            .into_iter()
            .map(|(field, source)| (field.to_string(), source))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string argument `{key}`"))?;
    if value.is_empty() {
        return Err(format!("missing required string argument `{key}`"));
    }
    Ok(value.to_string())
}

fn required_u64(args: &Value, key: &str) -> Result<u64, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;
    match object.get(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| format!("required argument `{key}` must be a u64")),
        Some(Value::String(value)) if !value.is_empty() => value
            .parse::<u64>()
            .map_err(|_| format!("required argument `{key}` must be a u64")),
        _ => Err(format!("missing required u64 argument `{key}`")),
    }
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(format!("optional argument `{key}` must be a string")),
    }
}

fn optional_attempt(args: &Value) -> Result<Option<String>, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;
    match object.get("attempt") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(_) => Err("optional argument `attempt` must be a string or number".to_string()),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod error_tests;

#[cfg(all(test, unix))]
mod checks_wait_tests;

#[cfg(test)]
mod test_helpers;
