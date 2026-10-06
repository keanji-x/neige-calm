//! `plugin_gitforge_publish` (#1830 S3, Planner-only): push `neige/track-<id>` to the checkout's
//! upstream URL and open (or reuse) its PR, only when the branch tip is the commit of a `done`
//! attempt of this track (`docs/architecture/1830-s3-push-pr-reclaim.md` D1–D7).
//!
//! The refusals (`refused: publish-…`, `-32409`, the repository's Conflict convention) are
//! answered before any operation exists. The publish itself is one kernel forge action under
//! `GIT_FORGE_PLUGIN_ID`, run in the track worktree by `PR_PUBLISH_SCRIPT` after the
//! credential split `FORGE_SHELL_PRELUDE`; the Planner waits for it and gets the PR inline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use calm_types::forge_git::FORGE_SHELL_PRELUDE;

use super::publish_scripts::{
    PR_PUBLISH_OUTPUT_PROBE_SCRIPT, PR_PUBLISH_PROBE_SCRIPT, PR_PUBLISH_SCRIPT,
};
use serde_json::{Value, json};

use crate::builtin_plugins::dev::PLUGIN_ID as GIT_FORGE_PLUGIN_ID;
use crate::event::{FieldSource, ForgeEventSpec};
use crate::git_candidate::candidate::{TrackCandidate, track_candidates};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    role_gated_write_annotations,
};
use crate::mcp_server::transport::forge_action_payload;
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action_with_key};
use crate::model::{CardRole, TaskStatus, Track, new_id};
use crate::operation::OperationOutcome;
use crate::operation::forge_action_adapter::ProbeSpec;
use crate::operation::workspace_lease::upstream::track_remote;
use crate::workspace_materialize::isolated_git_command;

/// The local tool name; it is served as `registry_name(PLUGIN_ID, PUBLISH)` (#2227).
pub(crate) const PUBLISH: &str = "publish";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(dev_publish_descriptor(), wrap(dev_publish));
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

fn dev_publish_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: PUBLISH.into(),
        description: include_str!("../../../prompts/tools/plugin_gitforge_publish.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["idempotency_key", "title", "body"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 },
                "body": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// Refusals carry no tool name: the registry leads each with the served name.
fn required_string(args: &Value, name: &str, non_empty: bool) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !non_empty || !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("missing `{name}`")))
}

fn refused(text: String) -> RpcError {
    RpcError::conflict(format!("refused: {text}"))
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::internal(error.to_string())
}

/// D3: the tip may be published only when a `done` attempt of this track produced it. `facts`
/// are the track's candidates, newest first.
fn check_tip(track_id: &str, tip: &str, facts: &[TrackCandidate]) -> Result<(), RpcError> {
    let at_tip: Vec<&TrackCandidate> = facts.iter().filter(|c| c.commit_sha == tip).collect();
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

async fn candidate_facts(
    ctx: &AppContext,
    track_id: &str,
) -> Result<Vec<TrackCandidate>, RpcError> {
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| internal("requires a sqlite-backed repo"))?;
    track_candidates(pool, track_id).await.map_err(internal)
}

/// What the publish pushes and where: the tip of the track branch, the upstream URL (both the
/// push destination and gh's `--repo`, D2) and the base branch — the track branch's own upstream
/// (#2112), never the branch the main checkout is on now.
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
    let track_id = track.id.to_string();
    tokio::task::spawn_blocking(move || {
        let (target, upstream) = track_remote(&track_id, &worktree).map_err(internal)?;
        let Some(upstream) = upstream else {
            return Err(refused(format!(
                "publish-no-upstream: {} has no upstream remote to push to; set one with git -C \
                 {} branch --set-upstream-to and retry",
                target.branch,
                target.path.display()
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

/// D5 and D6: the forge payload of one publish. `facts` are the track's candidates, whose
/// commits the script may replace on the remote (#2058 D1); argv is not part of the payload hash,
/// the event fields and the probes are.
fn publish_payload(
    dest: &Destination,
    title: &str,
    body: &str,
    idempotency_key: &str,
    facts: &[TrackCandidate],
) -> PluginForgePayload {
    let (sha, branch, url) = (dest.tip.as_str(), dest.branch.as_str(), dest.url.as_str());
    let own_commits = facts
        .iter()
        .map(|candidate| candidate.commit_sha.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    forge_action_payload(
        // The sha is in the payload (argv and probe), not the key: the same key replays its first
        // publish, and the same key over a moved tip is `idempotency_payload_conflict`.
        format!("track.publish:{idempotency_key}"),
        shell_argv(
            PR_PUBLISH_SCRIPT,
            &[sha, branch, url, &dest.base, title, body, &own_commits],
        ),
        publish_receipt_event(),
        ProbeSpec {
            probe_argv: shell_argv(PR_PUBLISH_PROBE_SCRIPT, &[sha, branch, url]),
            output_probe_argv: Some(shell_argv(
                PR_PUBLISH_OUTPUT_PROBE_SCRIPT,
                &[sha, branch, url],
            )),
        },
    )
}

/// The publish's synchronous receipt: `forge.pr.published` from the script's stdout. `pr_action` and `url` are
/// for the tool result; the event drops them.
fn publish_receipt_event() -> ForgeEventSpec {
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    ForgeEventSpec {
        event_kind: "forge.pr.published".into(),
        fields: [
            ("pr_number".to_string(), json_field("/number")),
            // `created`, `reused` or `recovered`.
            ("pr_action".to_string(), json_field("/pr_action")),
            ("head_sha".to_string(), json_field("/headRefOid")),
            // The PR's web URL.
            ("url".to_string(), json_field("/url")),
        ]
        .into_iter()
        .collect(),
    }
}

async fn dev_publish(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
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
        publish_payload(&dest, &title, &body, &idempotency_key, &facts),
        new_id(),
    )
    .await?
    .map_err(|error| {
        // The one submit refusal a Planner can cause: its key already names another commit.
        RpcError::conflict(format!(
            "refused: publish-key-reused: {error}; call again with a new idempotency_key"
        ))
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
        "pr_action": event["pr_action"],
        "head_sha": event["head_sha"],
        "branch": dest.branch,
        "base": dest.base,
        "url": event["url"],
    }))
}

/// A publish that ran and did not land. Its key is spent (operation keys are permanent).
fn publish_failed(op_id: &str, reason: &str) -> RpcError {
    RpcError::conflict(format!(
        "publish-failed: operation {op_id}: {reason}. Nothing more runs under this \
             idempotency_key: fix the cause, then call again with a new one."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_planner_only_and_declares_its_local_name() {
        let d = dev_publish_descriptor();
        assert_eq!(d.name, "publish");
        assert_eq!(d.roles, &[CardRole::Planner]);
        let mut registry = ToolRegistry::new();
        register_into(&mut registry);
        assert!(registry.lookup("publish").is_some());
        assert!(registry.lookup("neige_track_publish").is_none()); // retired-name: rejection input
    }

    /// #2139: a recovered publish re-reads the PR with the output probe; its stdout, from gh's
    /// indented export, extracts through the real event fields with `pr_action` `recovered`.
    #[test]
    fn the_output_probe_answers_every_event_field() {
        let dest = Destination {
            worktree: PathBuf::from("/nonexistent"),
            branch: "neige/track-t".into(),
            tip: "abc".into(),
            url: "/origin.git".into(),
            base: "main".into(),
        };
        let payload = publish_payload(&dest, "Title", "Body.", "k", &[]);
        let argv = payload.probe.unwrap().output_probe_argv.unwrap();
        // `argv[2]` is the prelude, a newline and the script; the fake gh replaces the prelude.
        let script = argv[2].strip_prefix(FORGE_SHELL_PRELUDE).unwrap();
        let fake_gh = "neige_gh() { printf '{\n  \"headRefOid\": \"abc\",\n  \"number\": 7,\n  \"url\": \"https://github.invalid/pull/7\"\n}\n'; }";
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{fake_gh}{script}"))
            .args(&argv[3..])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
        let fields = publish_receipt_event()
            .extract_payload(0, Some(&stdout))
            .unwrap();
        assert_eq!(fields["pr_action"], json!("recovered"));
        assert_eq!(fields["pr_number"], json!(7));
        assert_eq!(fields["head_sha"], json!("abc"));
        assert_eq!(fields["url"], json!("https://github.invalid/pull/7"));
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn native_publish_uses_a_synchronous_receipt_event() {
        let dest = Destination {
            worktree: PathBuf::from("/nonexistent"),
            branch: "neige/track-t".into(),
            tip: "abc".into(),
            url: "/origin.git".into(),
            base: "main".into(),
        };
        let payload = publish_payload(&dest, "Title", "Body.", "receipt", &[]);
        assert!(!payload.parked);
        assert_eq!(payload.event_spec.unwrap().event_kind, "forge.pr.published");
    }
}
