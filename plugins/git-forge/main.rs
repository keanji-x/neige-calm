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
        None,
        false,
    )
}

fn lower_gh_pr_create(args: &Value) -> Result<Value, String> {
    let repo = required_string(args, "repo")?;
    let head = required_string(args, "head")?;
    let base = required_string(args, "base")?;
    let title = required_string(args, "title")?;
    let body = required_string(args, "body")?;
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
            base,
            "--title".into(),
            title,
            "--body".into(),
            body,
        ],
        format!("gh.pr.create:{repo}:{head}"),
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
                "gh",
                "pr",
                "view",
                head,
                "--repo",
                repo,
                "--json",
                "state"
            ],
            "output_probe_argv": [
                "gh",
                "pr",
                "view",
                head,
                "--repo",
                repo,
                "--json",
                "number,headRefOid"
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

#[cfg(test)]
mod tests {
    use super::*;

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
                "probe": null,
                "parked": false
            })
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
    }

    #[test]
    fn lowers_gh_pr_create() {
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
                "idem_key": "gh.pr.create:owner/repo:feature",
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
                        "gh",
                        "pr",
                        "view",
                        "feature",
                        "--repo",
                        "owner/repo",
                        "--json",
                        "state"
                    ],
                    "output_probe_argv": [
                        "gh",
                        "pr",
                        "view",
                        "feature",
                        "--repo",
                        "owner/repo",
                        "--json",
                        "number,headRefOid"
                    ]
                },
                "parked": true
            })
        );
        assert_no_reserved_context(&payload, &["wave_id"]);
        assert!(payload["probe"]["output_probe_argv"].is_array());
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
}
