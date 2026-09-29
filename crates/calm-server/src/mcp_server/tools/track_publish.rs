//! `calm.track.publish` (#1830 S3, Planner-only): push `neige/track-<id>` to the checkout's
//! upstream URL and open (or reuse) its PR, only when the branch tip is the commit of a `done`
//! attempt of this track (`docs/architecture/1830-s3-push-pr-reclaim.md` D1–D7).
//!
//! The refusals (`refused: publish-…`, `-32409`, the repository's Conflict convention) are
//! answered before any operation exists. The publish itself is one kernel forge action under
//! `GIT_FORGE_PLUGIN_ID`, run in the track worktree by `GIT_TRACK_PUBLISH_SCRIPT` after the
//! credential split `FORGE_SHELL_PRELUDE`; the Planner waits for it and gets the PR inline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use calm_types::forge_git::{
    FORGE_SHELL_PRELUDE, GIT_TRACK_PUBLISH_OUTPUT_PROBE_SCRIPT, GIT_TRACK_PUBLISH_PROBE_SCRIPT,
    GIT_TRACK_PUBLISH_SCRIPT,
};
use serde_json::{Value, json};

use crate::event::{FieldSource, ForgeEventSpec};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::emit::{GIT_FORGE_PLUGIN_ID, worker_delivery_payload};
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action_with_key};
use crate::model::{CardRole, TaskStatus, Track, new_id};
use crate::operation::OperationOutcome;
use crate::operation::forge_action_adapter::ProbeSpec;
use crate::operation::workspace_lease::track_worktree::track_worktree_target;
use crate::operation::workspace_lease::upstream::head_upstream;
use crate::workspace_materialize::isolated_git_command;

pub const TOOL_TRACK_PUBLISH: &str = "calm.track.publish";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_publish_descriptor(), wrap(track_publish));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn track_publish_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_PUBLISH.into(),
        description: include_str!("../../../prompts/tools/calm.track.publish.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "title", "body"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 },
                "body": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn required_string(args: &Value, name: &str, non_empty: bool) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !non_empty || !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("track_publish: missing `{name}`")))
}

fn refused(text: String) -> RpcError {
    RpcError::custom(-32409, format!("refused: {text}"))
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::internal(format!("track_publish: {error}"))
}

/// One `task_candidates` row of the track with its attempt's status.
struct CandidateFact {
    attempt_id: String,
    commit_sha: String,
    status: String,
}

/// D3: the tip may be published only when a `done` attempt of this track produced it. `facts`
/// are the track's candidates, newest first.
fn check_tip(track_id: &str, tip: &str, facts: &[CandidateFact]) -> Result<(), RpcError> {
    let at_tip: Vec<&CandidateFact> = facts.iter().filter(|c| c.commit_sha == tip).collect();
    if at_tip
        .iter()
        .any(|c| c.status == TaskStatus::Done.wire_label())
    {
        return Ok(());
    }
    if let Some(candidate) = at_tip.first() {
        return Err(refused(format!(
            "publish-candidate-not-done: {tip} is the candidate of attempt {}, which is {}; only \
             a done attempt's commit can be published.",
            candidate.attempt_id, candidate.status
        )));
    }
    let latest = facts.first().map_or_else(
        || "no attempt has produced a candidate yet".to_string(),
        |c| {
            format!(
                "latest candidate {}, attempt {}, {}",
                c.commit_sha, c.attempt_id, c.status
            )
        },
    );
    Err(refused(format!(
        "publish-not-a-candidate: neige/track-{track_id} is at {tip}, which no attempt of this \
         track produced ({latest}). A commit made after the last attempt is not verified: let a \
         task produce it, or undo it."
    )))
}

async fn candidate_facts(ctx: &AppContext, track_id: &str) -> Result<Vec<CandidateFact>, RpcError> {
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| internal("requires a sqlite-backed repo"))?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.producer_attempt_id, c.commit_sha, t.status FROM task_candidates c \
         JOIN tasks t ON t.id = c.producer_attempt_id WHERE c.track_id = ?1 \
         ORDER BY c.created_at_ms DESC, c.rowid DESC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|(attempt_id, commit_sha, status)| CandidateFact {
            attempt_id,
            commit_sha,
            status,
        })
        .collect())
}

/// What the publish pushes and where: the tip of the track branch, the upstream URL (both the
/// push destination and gh's `--repo`, D2) and the base branch.
struct Destination {
    worktree: PathBuf,
    branch: String,
    tip: String,
    url: String,
    base: String,
}

/// D7 and D2, and the tip of D3. Local git only, on a blocking thread.
async fn destination(track: &Track) -> Result<Destination, RpcError> {
    let Some(worktree) = track.workspace.worktree.clone() else {
        return Err(refused(
            "publish-needs-track-worktree: only an attached track with its own worktree can be \
             published (a managed track has no remote)"
                .into(),
        ));
    };
    let target = track_worktree_target(track.id.as_str(), &worktree).map_err(internal)?;
    tokio::task::spawn_blocking(move || {
        let upstream = head_upstream(&target.repo_root).map_err(internal)?;
        let Some(upstream) = upstream.filter(|upstream| upstream.remote != ".") else {
            return Err(refused(format!(
                "publish-no-upstream: {} has no upstream remote to push to; set one with git \
                 branch --set-upstream-to and retry",
                target.repo_root.display()
            )));
        };
        let tip = branch_tip(&target.repo_root, &target.branch)?;
        let base = upstream
            .merge
            .strip_prefix("refs/heads/")
            .unwrap_or(&upstream.merge)
            .to_string();
        Ok(Destination {
            worktree: PathBuf::from(worktree),
            branch: target.branch,
            tip,
            url: upstream.url,
            base,
        })
    })
    .await
    .map_err(internal)?
}

fn branch_tip(repo_root: &Path, branch: &str) -> Result<String, RpcError> {
    let output = isolated_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--verify", "-q"])
        .arg(format!("refs/heads/{branch}^{{commit}}"))
        .output()
        .map_err(internal)?;
    if !output.status.success() {
        return Err(internal(format!(
            "{branch} does not resolve to a commit in {}",
            repo_root.display()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `sh -c "<prelude>\n<script>" sh <args…>`.
fn shell_argv(script: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("{FORGE_SHELL_PRELUDE}\n{script}"),
        "sh".to_string(),
    ];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv
}

/// D5 and D6: the forge payload of one publish.
fn publish_payload(
    dest: &Destination,
    title: &str,
    body: &str,
    idempotency_key: &str,
) -> PluginForgePayload {
    let (sha, branch, url) = (dest.tip.as_str(), dest.branch.as_str(), dest.url.as_str());
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    worker_delivery_payload(
        // The sha is in the payload (argv and probe), not the key: the same key replays its first
        // publish, and the same key over a moved tip is `idempotency_payload_conflict`.
        format!("track.publish:{idempotency_key}"),
        shell_argv(
            GIT_TRACK_PUBLISH_SCRIPT,
            &[sha, branch, url, &dest.base, title, body],
        ),
        ForgeEventSpec {
            event_kind: "forge.pr.opened".into(),
            fields: [
                ("pr_number".to_string(), json_field("/number")),
                ("head_sha".to_string(), json_field("/headRefOid")),
                // The PR's web URL; `forge.pr.opened` drops it, the tool result returns it.
                ("url".to_string(), json_field("/url")),
            ]
            .into_iter()
            .collect(),
        },
        ProbeSpec {
            probe_argv: shell_argv(GIT_TRACK_PUBLISH_PROBE_SCRIPT, &[sha, branch, url]),
            output_probe_argv: Some(shell_argv(
                GIT_TRACK_PUBLISH_OUTPUT_PROBE_SCRIPT,
                &[sha, branch, url],
            )),
        },
    )
}

async fn track_publish(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let idempotency_key = required_string(&args, "idempotency_key", true)?;
    let title = required_string(&args, "title", true)?;
    let body = required_string(&args, "body", false)?;

    let card = ctx
        .repo
        .card_get(&identity.card_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal(format!("bound card {} not found", identity.card_id)))?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(internal)?
        .ok_or_else(|| internal(format!("track {} not found", card.track_id.as_str())))?;
    let dest = destination(&track).await?;
    let facts = candidate_facts(&ctx, track.id.as_str()).await?;
    check_tip(track.id.as_str(), &dest.tip, &facts)?;

    let Some(runtime) = ctx.operation_runtime.get().cloned() else {
        return Err(internal("operation runtime not bound"));
    };
    let submitted = submit_forge_action_with_key(
        &runtime,
        &ctx.gate_logs_dir,
        GIT_FORGE_PLUGIN_ID,
        track.id.as_str().to_string(),
        identity.card_id.clone(),
        dest.worktree.clone(),
        publish_payload(&dest, &title, &body, &idempotency_key),
        new_id(),
    )
    .await?
    .map_err(|error| {
        // The one submit refusal a Planner can cause: its key already names another commit.
        RpcError::custom(
            -32409,
            format!("refused: publish-key-reused: {error}; call again with a new idempotency_key"),
        )
    })?;
    let result = runtime.wait(&submitted.op_id).await.map_err(internal)?;
    let op_id = result.op_id;
    let event = match result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => result["event"].clone(),
        OperationOutcome::Failed { last_error, .. } => {
            return Err(publish_failed(&op_id, &last_error));
        }
        OperationOutcome::Stuck { reason, .. } => return Err(publish_failed(&op_id, &reason)),
    };
    Ok(json!({
        "ok": true,
        "op_id": op_id,
        "pr_number": event["pr_number"],
        "head_sha": event["head_sha"],
        "branch": dest.branch,
        "base": dest.base,
        "url": event["url"],
    }))
}

/// A publish that ran and did not land. Its key is spent (operation keys are permanent).
fn publish_failed(op_id: &str, reason: &str) -> RpcError {
    RpcError::custom(
        -32409,
        format!(
            "publish-failed: operation {op_id}: {reason}. Nothing more runs under this \
             idempotency_key: fix the cause, then call again with a new one."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_planner_only_and_named() {
        let d = track_publish_descriptor();
        assert_eq!(d.name, TOOL_TRACK_PUBLISH);
        assert_eq!(d.visible_to_roles, &[CardRole::Planner]);
    }
}
