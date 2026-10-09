use std::collections::BTreeMap;

use calm_types::event::{FieldSource, ForgeEventSpec};
use calm_types::forge_git::{
    FORGE_SHELL_PRELUDE, GIT_COMMIT_OUTPUT_PROBE_SCRIPT, GIT_COMMIT_PROBE_SCRIPT, GIT_COMMIT_SCRIPT,
};
use serde_json::{Value, json};

use crate::forge_caller::ForgeCallerScope;

mod checks;
mod checks_compat;
mod checks_pre2363;
use checks::*;
mod issue;
mod issue_creation;

pub fn lower(tool: &str, args: &Value) -> Result<Value, String> {
    match tool {
        "git_worktree_add" => lower_git_worktree_add(args),
        "git_commit" => lower_git_commit(args),
        "gh_pr_list" => lower_gh_pr_list(args),
        "gh_pr_diff" => lower_gh_pr_diff(args),
        "gh_pr_checks" => lower_gh_pr_checks(args),
        "gh_pr_merge" => lower_gh_pr_merge(args),
        "gh_issue_view" => lower_gh_issue_view(args),
        "gh_issue_close" => lower_gh_issue_close(args),
        "gh_issue_comment" => Err("issue comments require trusted forge caller metadata".into()),
        "gh_issue_comments" => issue::comments(args),
        "gh_issue_create" => Err("issue creation requires trusted forge caller metadata".into()),
        "gh_issue_search" => issue_creation::search(args),
        _ => Err(format!("unknown {} tool `{tool}`", super::PLUGIN_ID)),
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
        return Err(format!(
            "forge caller plugin does not match the {} plugin",
            super::PLUGIN_ID
        ));
    }
    match tool {
        "gh_issue_comment" => issue::comment(args, caller),
        "gh_issue_create" => issue_creation::create(args, caller),
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

/// $1 repository selector, $2 base SHA, $3 head SHA. The three-dot compare is the pull request's
/// net diff (merge base to head) of the requested commits, not of the PR's current head. An HTTP
/// error, such as an unknown commit or an oversized comparison, exits non-zero.
const PR_DIFF_READ_SCRIPT: &str = concat!(
    "identity=$(gh repo view \"$1\" --json nameWithOwner,url --jq '(.url | split(\"/\")[2]), .nameWithOwner') || exit 1\n",
    "host=${identity%%\n*}\nrepo=${identity#*\n}\n",
    "exec gh api --hostname \"$host\" -H 'Accept: application/vnd.github.diff' \"repos/$repo/compare/$2...$3\"\n",
);

fn lower_gh_pr_diff(args: &Value) -> Result<Value, String> {
    // Idempotent read: intentionally probe-free.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let base_sha = required_commit_sha(args, "base_sha")?;
    let head_sha = required_commit_sha(args, "head_sha")?;
    forge_payload(
        vec![
            "sh".into(),
            "-c".into(),
            PR_DIFF_READ_SCRIPT.into(),
            "sh".into(),
            repo.clone(),
            base_sha.clone(),
            head_sha.clone(),
        ],
        // v2: v1 results hold per-commit patches of whatever head the PR had then.
        format!("gh.pr.diff:v2:{repo}:{pr}:{base_sha}:{head_sha}"),
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
    let idem_key = format!(
        "gh.issue.view:v4:{}",
        json!([repo, issue, optional_attempt(args)?])
    );
    issue::read_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "view".into(),
            issue.to_string(),
            "--repo".into(),
            repo.clone(),
            "--json".into(),
            "number,url,state,title,body,labels".into(),
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

/// A full commit id (SHA-1 or SHA-256); the value is placed in an API path.
fn required_commit_sha(args: &Value, key: &str) -> Result<String, String> {
    let value = required_string(args, key)?;
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{key} must be a full commit SHA"));
    }
    Ok(value)
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
