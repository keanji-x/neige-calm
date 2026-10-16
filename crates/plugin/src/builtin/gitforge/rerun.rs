//! `plugin_gitforge_gh_run_rerun` (#2617 item E, Planner-only): re-run the failed jobs of
//! one completed workflow run attempt of a PR's head. The call is approval-gated: the kernel runs
//! it only after the user clicked Approve on the ask it raises for this exact request, so nothing
//! is posted on the Planner's word alone. The script checks the PR head and the run attempt before
//! the one POST; there is no landed probe, so a lost result is reported as unknown and is never
//! posted again under the same key.

use super::git_actions::{required_commit_sha, required_string, required_u64};
use super::publish::{ExecuteOutcome, Services};
use crate::{builtin::tools::NativeToolSpec, mcp::RpcError};
use calm_types::{
    forge_action::{ForgeApprovalRequest, PluginForgePayload},
    model::CardRole,
};
use serde_json::{Map, Value, json};

pub const RERUN_FAILED: &str = "gh_run_rerun";

/// The check-then-POST script, after the shared Actions prelude.
const RERUN_FAILED_SCRIPT: &str = concat!(
    include_str!("git_actions/actions_prelude.sh"),
    include_str!("rerun_failed.sh"),
);

/// Upper bounds, checked on parse, that keep the approval title within the ask's title bound.
const MAX_REPO_CHARS: usize = 200;
const MAX_REASON_CHARS: usize = 1000;

pub fn descriptor() -> NativeToolSpec {
    NativeToolSpec {
        name: RERUN_FAILED.into(),
        description: include_str!("prompts/plugin_gitforge_gh_run_rerun.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["repo", "pr", "head_sha", "run_id", "run_attempt", "reason", "idem"],
            "properties": {
                "repo": { "type": "string" },
                "pr": { "type": "integer", "minimum": 1 },
                "head_sha": { "type": "string" },
                "run_id": { "type": "integer", "minimum": 1 },
                "run_attempt": { "type": "integer", "minimum": 1 },
                "reason": { "type": "string" },
                "idem": { "type": "string" }
            }
        }),
        annotations: Some(calm_types::plugin::role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// One validated request.
#[derive(Debug)]
struct Rerun {
    repo: String,
    pr: u64,
    head_sha: String,
    run_id: u64,
    run_attempt: u64,
    reason: String,
    idem: String,
}

fn bounded(args: &Value, key: &str, max: usize) -> Result<String, String> {
    let value = required_string(args, key)?;
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(format!("`{key}` must be 1 to {max} characters"));
    }
    Ok(value)
}

fn positive(args: &Value, key: &str) -> Result<u64, String> {
    match required_u64(args, key)? {
        0 => Err(format!("`{key}` must be at least 1")),
        value => Ok(value),
    }
}

impl Rerun {
    fn parse(args: &Value) -> Result<Self, String> {
        Ok(Self {
            repo: bounded(args, "repo", MAX_REPO_CHARS)?,
            pr: positive(args, "pr")?,
            // GitHub reports head SHAs in lowercase and the script compares them exactly.
            head_sha: required_commit_sha(args, "head_sha")?.to_ascii_lowercase(),
            run_id: positive(args, "run_id")?,
            run_attempt: positive(args, "run_attempt")?,
            reason: bounded(args, "reason", MAX_REASON_CHARS)?,
            idem: required_string(args, "idem")?,
        })
    }

    /// The forge payload. The key names the run attempt and the caller's idem; the PR, head and
    /// reason are in the context, so the payload hash the approval binds covers them too.
    fn payload(&self) -> PluginForgePayload {
        let key = serde_json::to_string(&json!([
            self.repo,
            self.run_id,
            self.run_attempt,
            self.idem
        ]))
        .expect("a JSON array of strings and numbers");
        let mut context = Map::new();
        context.insert("pr".into(), json!(self.pr));
        context.insert("head_sha".into(), json!(self.head_sha));
        context.insert("reason".into(), json!(self.reason));
        PluginForgePayload {
            argv: [
                "sh",
                "-c",
                RERUN_FAILED_SCRIPT,
                "sh",
                &self.repo,
                &self.pr.to_string(),
                &self.head_sha,
                &self.run_id.to_string(),
                &self.run_attempt.to_string(),
            ]
            .map(str::to_string)
            .to_vec(),
            idem_key: format!("gh.run.rerun_failed:v1:{key}"),
            event_spec: None,
            subject: None,
            context,
            probe: None,
            parked: false,
            replay_any_version: false,
            approval: Some(ForgeApprovalRequest {
                title: format!(
                    "Re-run failed jobs of run {} attempt {}, {} PR #{} head {}? Reason: {}",
                    self.run_id,
                    self.run_attempt,
                    self.repo,
                    self.pr,
                    self.head_sha,
                    self.reason.trim()
                ),
            }),
        }
    }

    /// The `gh_pr_checks` fence that waits for this rerun's result.
    fn after_rerun(&self) -> Value {
        json!({ "run_id": self.run_id, "run_attempt": self.run_attempt })
    }
}

fn refused(text: String) -> RpcError {
    RpcError::conflict(format!("refused: {text}"))
}

pub async fn call<S: Services>(
    services: &S,
    card_id: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let rerun = Rerun::parse(&args).map_err(RpcError::invalid_params)?;
    let (track_id, cwd) = services.planner_cwd(card_id).await?;
    let wait = format!(
        "wait for CI with plugin_gitforge_gh_pr_checks (repo, pr, a new attempt, after_rerun: {})",
        rerun.after_rerun()
    );
    match services
        .execute(&track_id, card_id, cwd, rerun.payload())
        .await?
    {
        ExecuteOutcome::Succeeded { op_id, .. } => Ok(json!({
            "ok": true,
            "op_id": op_id,
            "run_id": rerun.run_id,
            "rerun_of_attempt": rerun.run_attempt,
            "after_rerun": rerun.after_rerun(),
            "next": format!("GitHub accepted the rerun; {wait}."),
        })),
        ExecuteOutcome::AwaitingApproval { ask_id } => Ok(json!({
            "ok": false,
            "status": "awaiting_approval",
            "ask_id": ask_id,
            "next": "Nothing was posted: the user is asked to approve this rerun. When the \
                     answer wakes you, call again with exactly the same arguments; do not ask \
                     separately.",
        })),
        ExecuteOutcome::KeyReused { reason } => Err(refused(format!(
            "rerun-key-reused: {reason}; this idem holds another request, call with a new idem"
        ))),
        ExecuteOutcome::Declined { ask_id } => Err(refused(format!(
            "rerun-declined: the user declined this rerun in ask {ask_id}; nothing was posted. \
             A new request needs a new idem and a new approval."
        ))),
        ExecuteOutcome::Failed { op_id, reason } => Err(refused(format!(
            "rerun-failed: operation {op_id}: {reason}. Exit 20: the PR head is not head_sha; \
             21: the run is not a completed attempt run_attempt of that head and repository; \
             22: a check could not be read. Nothing was posted for these. Any other exit is the \
             rerun request itself failing, and whether GitHub took it is unknown: do not \
             request it again; {wait}."
        ))),
        ExecuteOutcome::Unknown { op_id, reason } => Err(refused(format!(
            "rerun-unknown: operation {op_id}: {reason}. Whether GitHub took the rerun request \
             is unknown, and it is never posted again under this idem: do not request it \
             again; {wait}."
        ))),
    }
}

#[cfg(test)]
mod tests;
