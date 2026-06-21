use std::collections::BTreeMap;
use std::io::{BufRead, BufWriter, Write};

use calm_types::event::{FieldSource, ForgeEventSpec};
use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => return,
        };
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("git-forge: bad json: {e}");
                continue;
            }
        };
        let Some(id) = frame.get("id").cloned() else {
            continue;
        };
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let reply = match method {
            "initialize" => initialize_reply(&frame, id),
            "tools/call" => tools_call_reply(&frame, id),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "echo": method }
            }),
        };

        let mut encoded = serde_json::to_string(&reply).expect("reply serializes");
        encoded.push('\n');
        if out.write_all(encoded.as_bytes()).is_err() {
            return;
        }
        if out.flush().is_err() {
            return;
        }
    }
}

fn initialize_reply(frame: &Value, id: Value) -> Value {
    let protocol = frame
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .cloned()
        .unwrap_or_else(|| Value::String("2025-11-25".into()));
    let expected = frame
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let echoed = std::env::var("NEIGE_PLUGIN_TOKEN")
        .ok()
        .or(expected)
        .unwrap_or_default();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol,
            "serverInfo": { "name": "git-forge", "version": "0.1.0" },
            "capabilities": {
                "experimental": {
                    "dev.neige/kernel-callbacks": { "version": 1 }
                }
            },
            "_meta": {
                "dev.neige/auth": { "echoed_token": echoed }
            }
        }
    })
}

fn tools_call_reply(frame: &Value, id: Value) -> Value {
    let tool = frame
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = frame
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match lower(tool, &args) {
        Ok(structured) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [],
                "isError": false,
                "structuredContent": structured
            }
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": error }],
                "isError": true,
                "structuredContent": { "error": error }
            }
        }),
    }
}

fn lower(tool: &str, args: &Value) -> Result<Value, String> {
    match tool {
        "git.worktree.add" => lower_git_worktree_add(args),
        "git.commit" => lower_git_commit(args),
        "gh.pr.create" => lower_gh_pr_create(args),
        "gh.pr.list" => lower_gh_pr_list(args),
        "gh.pr.diff" => lower_gh_pr_diff(args),
        "gh.pr.checks" => lower_gh_pr_checks(args),
        "gh.pr.merge" => lower_gh_pr_merge(args),
        "gh.issue.view" => lower_gh_issue_view(args),
        "gh.issue.close" => lower_gh_issue_close(args),
        _ => Err(format!("unknown git-forge tool `{tool}`")),
    }
}

fn lower_git_worktree_add(args: &Value) -> Result<Value, String> {
    let target = required_string(args, "target")?;
    let branch = optional_string(args, "branch")?;
    // ③-c finalizes worktree anchoring/collision/ordering
    let mut argv = vec![
        "git".to_string(),
        "worktree".to_string(),
        "add".to_string(),
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
    forge_payload(
        vec!["git".into(), "commit".into(), "-m".into(), message],
        format!("git.commit:{idem}"),
        None,
        json!({}),
        Some(json!({
            // Idempotent contract: after a nonzero `git commit`, an empty index
            // means the requested commit landed already or there was nothing to commit.
            "probe_argv": [
                "sh",
                "-c",
                GIT_COMMIT_PROBE_SCRIPT,
                "sh"
            ]
        })),
        false,
    )
}

fn lower_gh_pr_create(args: &Value) -> Result<Value, String> {
    let repo = required_string(args, "repo")?;
    let head = required_string(args, "head")?;
    let base = required_string(args, "base")?;
    let title = required_string(args, "title")?;
    let body = required_string(args, "body")?;
    let idem_key = format!("gh.pr.create:{repo}:{base}:{head}");
    forge_payload(
        vec![
            "gh".into(),
            "pr".into(),
            "create".into(),
            "--repo".into(),
            repo.clone(),
            "--head".into(),
            head.clone(),
            "--base".into(),
            base.clone(),
            "--title".into(),
            title,
            "--body".into(),
            body,
        ],
        idem_key,
        Some(event_spec(
            "forge.pr.opened",
            [
                (
                    "pr_number",
                    FieldSource::JsonField {
                        path: "/number".into(),
                    },
                ),
                (
                    "head_sha",
                    FieldSource::JsonField {
                        path: "/headRefOid".into(),
                    },
                ),
            ],
        )),
        json!({}),
        Some(json!({
            "probe_argv": [
                "sh",
                "-c",
                PR_CREATE_PROBE_SCRIPT,
                "sh",
                head,
                repo,
                base
            ],
            "output_probe_argv": [
                "gh",
                "pr",
                "list",
                "--repo",
                repo,
                "--head",
                head,
                "--base",
                base,
                "--state",
                "open",
                "--json",
                "number,headRefOid",
                "--jq",
                ".[0]"
            ]
        })),
        true,
    )
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
        true,
    )
}

fn lower_gh_pr_checks(args: &Value) -> Result<Value, String> {
    // Idempotent read: no mutating landed-verdict probe is attached.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let attempt = optional_attempt(args)?;
    let idem_key = match attempt {
        Some(attempt) => format!("gh.pr.checks:{repo}:{pr}:{attempt}"),
        None => format!("gh.pr.checks:{repo}:{pr}"),
    };
    let argv = vec![
        "gh".into(),
        "pr".into(),
        "view".into(),
        pr.to_string(),
        "--repo".into(),
        repo.clone(),
        "--json".into(),
        "statusCheckRollup".into(),
        "--jq".into(),
        "{conclusion: ([.statusCheckRollup[] | .conclusion // .state // .status // empty] | if any(. == \"FAILURE\" or . == \"ERROR\" or . == \"TIMED_OUT\" or . == \"CANCELLED\") then \"failure\" elif any(. == \"PENDING\" or . == \"QUEUED\" or . == \"IN_PROGRESS\" or . == \"EXPECTED\") then \"pending\" else \"success\" end)}".into(),
    ];
    forge_payload(
        argv.clone(),
        idem_key,
        Some(event_spec(
            "forge.pr.checks",
            [(
                "conclusion",
                FieldSource::JsonField {
                    path: "/conclusion".into(),
                },
            )],
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
            "output_probe_argv": argv
        })),
        true,
    )
}

fn lower_gh_pr_merge(args: &Value) -> Result<Value, String> {
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let phase = required_string(args, "phase")?;
    let slice_id = required_string(args, "slice_id")?;
    let mut payload = forge_payload(
        vec![
            "gh".into(),
            "pr".into(),
            "merge".into(),
            pr.to_string(),
            "--repo".into(),
            repo.clone(),
            "--squash".into(),
            "--delete-branch".into(),
        ],
        format!("gh.pr.merge:{repo}:{pr}"),
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
            "probe_argv": [
                "sh",
                "-c",
                PR_MERGE_PROBE_SCRIPT,
                "sh",
                pr.to_string(),
                repo
            ],
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
    payload["subject"] = json!({
        "phase": phase,
        "slice_id": slice_id,
        "pr_number": pr
    });
    Ok(payload)
}

fn lower_gh_issue_view(args: &Value) -> Result<Value, String> {
    // Idempotent read: intentionally probe-free.
    let repo = required_string(args, "repo")?;
    let issue = required_u64(args, "issue")?;
    forge_payload(
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
        format!("gh.issue.view:{repo}:{issue}"),
        Some(event_spec("forge.issue.read", [])),
        json!({"issue_number": issue}),
        None,
        false,
    )
}

const ISSUE_CLOSE_PROBE_SCRIPT: &str = "out=$(gh issue view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; \
     case \"$out\" in *'\"state\":\"CLOSED\"'*) exit 0 ;; *) exit 1 ;; esac";
const PR_MERGE_PROBE_SCRIPT: &str = "out=$(gh pr view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; \
     case \"$out\" in *'\"state\":\"MERGED\"'*) exit 0 ;; *) exit 1 ;; esac";
const PR_CREATE_PROBE_SCRIPT: &str = "n=$(gh pr list --repo \"$2\" --head \"$1\" --base \"$3\" --state open --json number --jq 'length' 2>/dev/null) || exit 3; \
     case \"$n\" in '') exit 3 ;; 0) exit 1 ;; *) exit 0 ;; esac";
const GIT_COMMIT_PROBE_SCRIPT: &str = "git rev-parse --verify HEAD >/dev/null 2>&1 || exit 3; \
     if git diff --cached --quiet 2>/dev/null; then exit 0; else exit 1; fi";

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
mod tests {
    use super::*;
    use calm_server::operation::forge_action_adapter::SUPPORTED_FORGE_EVENT_KINDS;

    #[test]
    fn lowers_git_worktree_add() {
        let payload = lower(
            "git.worktree.add",
            &json!({ "target": "/tmp/wt", "branch": "wt-x" }),
        )
        .expect("lower worktree add");
        assert_eq!(
            payload,
            json!({
                "argv": ["git", "worktree", "add", "/tmp/wt", "-b", "wt-x"],
                "idem_key": "git.worktree.add:/tmp/wt",
                "event_spec": {
                    "event_kind": "worktree.provisioned",
                    "fields": {}
                },
                "subject": null,
                "context": { "path": "/tmp/wt" },
                "probe": null,
                "parked": false
            })
        );
        assert_no_reserved_context(&payload, &["wave_id", "card_id"]);
    }

    #[test]
    fn lowers_git_commit() {
        let expected_probe_script = "git rev-parse --verify HEAD >/dev/null 2>&1 || exit 3; if git diff --cached --quiet 2>/dev/null; then exit 0; else exit 1; fi";
        let payload = lower("git.commit", &json!({ "message": "m", "idem": "step-1" }))
            .expect("lower commit");
        assert_eq!(
            payload,
            json!({
                "argv": ["git", "commit", "-m", "m"],
                "idem_key": "git.commit:step-1",
                "event_spec": null,
                "subject": null,
                "context": {},
                "probe": {
                    "probe_argv": [
                        "sh",
                        "-c",
                        expected_probe_script,
                        "sh"
                    ]
                },
                "parked": false
            })
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
    }

    #[test]
    fn lowers_gh_pr_create() {
        let expected_probe_script = "n=$(gh pr list --repo \"$2\" --head \"$1\" --base \"$3\" --state open --json number --jq 'length' 2>/dev/null) || exit 3; case \"$n\" in '') exit 3 ;; 0) exit 1 ;; *) exit 0 ;; esac";
        let payload = lower(
            "gh.pr.create",
            &json!({
                "repo": "owner/repo",
                "head": "feature",
                "base": "main",
                "title": "Title",
                "body": "Body"
            }),
        )
        .expect("lower gh pr create");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "pr",
                    "create",
                    "--repo",
                    "owner/repo",
                    "--head",
                    "feature",
                    "--base",
                    "main",
                    "--title",
                    "Title",
                    "--body",
                    "Body"
                ],
                "idem_key": "gh.pr.create:owner/repo:main:feature",
                "event_spec": {
                    "event_kind": "forge.pr.opened",
                    "fields": {
                        "head_sha": { "json_field": { "path": "/headRefOid" } },
                        "pr_number": { "json_field": { "path": "/number" } }
                    }
                },
                "subject": null,
                "context": {},
                "probe": {
                    "probe_argv": [
                        "sh",
                        "-c",
                        expected_probe_script,
                        "sh",
                        "feature",
                        "owner/repo",
                        "main"
                    ],
                    "output_probe_argv": [
                        "gh",
                        "pr",
                        "list",
                        "--repo",
                        "owner/repo",
                        "--head",
                        "feature",
                        "--base",
                        "main",
                        "--state",
                        "open",
                        "--json",
                        "number,headRefOid",
                        "--jq",
                        ".[0]"
                    ]
                },
                "parked": true
            })
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
        assert_supported_event_kind(&payload);
        assert!(payload["probe"]["output_probe_argv"].is_array());
    }

    #[test]
    fn lowers_gh_pr_list() {
        let payload = lower(
            "gh.pr.list",
            &json!({
                "repo": "owner/repo",
                "base": "main",
                "head": "feature"
            }),
        )
        .expect("lower gh pr list");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "pr",
                    "list",
                    "--repo",
                    "owner/repo",
                    "--base",
                    "main",
                    "--head",
                    "feature",
                    "--state",
                    "open",
                    "--json",
                    "number",
                    "--jq",
                    "[.[].number]"
                ],
                "idem_key": "gh.pr.list:owner/repo:main:feature",
                "event_spec": {
                    "event_kind": "forge.scan.completed",
                    "fields": {
                        "overlapping_prs": { "json_field": { "path": "" } }
                    }
                },
                "subject": null,
                "context": {},
                "probe": {
                    "probe_argv": [
                        "gh",
                        "pr",
                        "list",
                        "--repo",
                        "owner/repo",
                        "--limit",
                        "1"
                    ],
                    "output_probe_argv": [
                        "gh",
                        "pr",
                        "list",
                        "--repo",
                        "owner/repo",
                        "--base",
                        "main",
                        "--head",
                        "feature",
                        "--state",
                        "open",
                        "--json",
                        "number",
                        "--jq",
                        "[.[].number]"
                    ]
                },
                "parked": true
            })
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
        assert_supported_event_kind(&payload);
    }

    #[test]
    fn lowers_gh_pr_diff() {
        let payload = lower(
            "gh.pr.diff",
            &json!({
                "repo": "owner/repo",
                "pr": "42",
                "base_sha": "base123",
                "head_sha": "head456"
            }),
        )
        .expect("lower gh pr diff");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "pr",
                    "diff",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--patch"
                ],
                "idem_key": "gh.pr.diff:owner/repo:42:base123:head456",
                "event_spec": {
                    "event_kind": "forge.pr.diff.read",
                    "fields": {}
                },
                "subject": null,
                "context": {
                    "pr_number": 42,
                    "base_sha": "base123",
                    "head_sha": "head456"
                },
                "probe": null,
                "parked": true
            })
        );
        assert_no_reserved_context(&payload, &["wave_id", "artifact_path"]);
        assert_supported_event_kind(&payload);
    }

    #[test]
    fn lowers_gh_pr_checks() {
        let payload = lower(
            "gh.pr.checks",
            &json!({
                "repo": "owner/repo",
                "pr": 42
            }),
        )
        .expect("lower gh pr checks");
        let attempt_payload = lower(
            "gh.pr.checks",
            &json!({
                "repo": "owner/repo",
                "pr": 42,
                "attempt": 7
            }),
        )
        .expect("lower gh pr checks with attempt");
        let jq = "{conclusion: ([.statusCheckRollup[] | .conclusion // .state // .status // empty] | if any(. == \"FAILURE\" or . == \"ERROR\" or . == \"TIMED_OUT\" or . == \"CANCELLED\") then \"failure\" elif any(. == \"PENDING\" or . == \"QUEUED\" or . == \"IN_PROGRESS\" or . == \"EXPECTED\") then \"pending\" else \"success\" end)}";
        let expected_payload = |idem_key: &str| {
            json!({
                "argv": [
                    "gh",
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "statusCheckRollup",
                    "--jq",
                    jq
                ],
                "idem_key": idem_key,
                "event_spec": {
                    "event_kind": "forge.pr.checks",
                    "fields": {
                        "conclusion": { "json_field": { "path": "/conclusion" } }
                    }
                },
                "subject": null,
                "context": { "pr_number": 42 },
                "probe": {
                    "probe_argv": [
                        "gh",
                        "pr",
                        "view",
                        "42",
                        "--repo",
                        "owner/repo",
                        "--json",
                        "state"
                    ],
                    "output_probe_argv": [
                        "gh",
                        "pr",
                        "view",
                        "42",
                        "--repo",
                        "owner/repo",
                        "--json",
                        "statusCheckRollup",
                        "--jq",
                        jq
                    ]
                },
                "parked": true
            })
        };
        assert_eq!(payload, expected_payload("gh.pr.checks:owner/repo:42"));
        assert_eq!(
            attempt_payload,
            expected_payload("gh.pr.checks:owner/repo:42:7")
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
        assert_no_reserved_context(&attempt_payload, &["wave_id"]);
        assert_supported_event_kind(&payload);
        assert_supported_event_kind(&attempt_payload);
    }

    #[test]
    fn lowers_gh_pr_merge() {
        let expected_probe_script = "out=$(gh pr view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; case \"$out\" in *'\"state\":\"MERGED\"'*) exit 0 ;; *) exit 1 ;; esac";
        let payload = lower(
            "gh.pr.merge",
            &json!({
                "repo": "owner/repo",
                "pr": 42,
                "phase": "impl",
                "slice_id": "809"
            }),
        )
        .expect("lower gh pr merge");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "pr",
                    "merge",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--squash",
                    "--delete-branch"
                ],
                "idem_key": "gh.pr.merge:owner/repo:42",
                "event_spec": {
                    "event_kind": "forge.pr.merged",
                    "fields": {
                        "head_sha": { "json_field": { "path": "/headRefOid" } },
                        "merge_sha": { "json_field": { "path": "/mergeCommit/oid" } }
                    }
                },
                "subject": {
                    "phase": "impl",
                    "slice_id": "809",
                    "pr_number": 42
                },
                "context": {},
                "probe": {
                    "probe_argv": [
                        "sh",
                        "-c",
                        expected_probe_script,
                        "sh",
                        "42",
                        "owner/repo"
                    ],
                    "output_probe_argv": [
                        "gh",
                        "pr",
                        "view",
                        "42",
                        "--repo",
                        "owner/repo",
                        "--json",
                        "headRefOid,mergeCommit"
                    ]
                },
                "parked": true
            })
        );
        assert_no_reserved_context(&payload, &["wave_id", "subject"]);
        assert_supported_event_kind(&payload);
    }

    #[test]
    fn lowers_gh_issue_view() {
        let payload = lower(
            "gh.issue.view",
            &json!({
                "repo": "owner/repo",
                "issue": "808"
            }),
        )
        .expect("lower gh issue view");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "issue",
                    "view",
                    "808",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "body",
                    "--jq",
                    ".body"
                ],
                "idem_key": "gh.issue.view:owner/repo:808",
                "event_spec": {
                    "event_kind": "forge.issue.read",
                    "fields": {}
                },
                "subject": null,
                "context": {
                    "issue_number": 808
                },
                "probe": null,
                "parked": false
            })
        );
        assert_no_reserved_context(&payload, &["wave_id", "artifact_path"]);
        assert_supported_event_kind(&payload);
    }

    #[test]
    fn lowers_gh_issue_close() {
        let expected_probe_script = "out=$(gh issue view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; case \"$out\" in *'\"state\":\"CLOSED\"'*) exit 0 ;; *) exit 1 ;; esac";
        let payload = lower(
            "gh.issue.close",
            &json!({
                "repo": "owner/repo",
                "issue": 808
            }),
        )
        .expect("lower gh issue close");
        assert_eq!(
            payload,
            json!({
                "argv": [
                    "gh",
                    "issue",
                    "close",
                    "808",
                    "--repo",
                    "owner/repo"
                ],
                "idem_key": "gh.issue.close:owner/repo:808",
                "event_spec": {
                    "event_kind": "forge.issue.closed",
                    "fields": {}
                },
                "subject": null,
                "context": { "issue_number": 808 },
                "probe": {
                    "probe_argv": [
                        "sh",
                        "-c",
                        expected_probe_script,
                        "sh",
                        "808",
                        "owner/repo"
                    ]
                },
                "parked": true
            })
        );
        assert!(payload["probe"]["output_probe_argv"].is_null());
        assert_no_reserved_context(&payload, &["wave_id"]);
        assert_supported_event_kind(&payload);
    }

    #[test]
    fn rejects_unknown_tool() {
        let err = lower("git.push", &json!({})).expect_err("unknown tool rejected");
        assert!(err.contains("unknown git-forge tool"));
    }

    #[test]
    fn rejects_missing_required_arg() {
        let err = lower("git.commit", &json!({ "message": "m" }))
            .expect_err("missing required argument rejected");
        assert!(err.contains("idem"));
    }

    fn assert_no_reserved_context(payload: &Value, reserved: &[&str]) {
        let context = payload["context"].as_object().expect("context object");
        for key in reserved {
            assert!(
                !context.contains_key(*key),
                "context must not contain reserved key `{key}`"
            );
        }
        if let Some(fields) = payload
            .pointer("/event_spec/fields")
            .and_then(Value::as_object)
        {
            for key in reserved {
                assert!(
                    !fields.contains_key(*key),
                    "event fields must not contain reserved key `{key}`"
                );
            }
        }
    }

    fn assert_supported_event_kind(payload: &Value) {
        let event_kind = payload
            .pointer("/event_spec/event_kind")
            .and_then(Value::as_str)
            .expect("payload carries event kind");
        assert!(
            SUPPORTED_FORGE_EVENT_KINDS.contains(&event_kind),
            "unsupported event kind `{event_kind}`"
        );
    }
}
