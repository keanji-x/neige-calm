//! Kernel facts and execution adapter for the owning Gitforge publish service.
use super::PLUGIN_ID as GIT_FORGE_PLUGIN_ID;
use crate::mcp_server::{
    framing::RpcError,
    registry::{AppContext, ToolDescriptor, ToolRegistry},
    transport::{PluginForgePayload, submit_forge_action_with_key},
};
use crate::operation::workspace_lease::upstream::track_remote;
use crate::{
    git_candidate::candidate::{TrackCandidate, track_candidates},
    model::{TaskStatus, Track, new_id},
    operation::OperationOutcome,
    workspace_materialize::isolated_git_command,
};
use async_trait::async_trait;
#[cfg(test)]
pub(crate) use plugin::builtin::gitforge::publish::PUBLISH;
use plugin::builtin::gitforge::publish::{self, Candidate, Destination, PublishOutcome, Services};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}
fn refused(message: String) -> RpcError {
    RpcError::conflict(format!("refused: {message}"))
}
#[async_trait]
impl Services for AppContext {
    async fn destination(&self, card_id: &str) -> Result<(String, Destination), RpcError> {
        let card = self
            .repo
            .card_get(card_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal(format!("bound card {card_id} not found")))?;
        let track = self
            .repo
            .track_get(card.track_id.as_str())
            .await
            .map_err(internal)?
            .ok_or_else(|| internal(format!("track {} not found", card.track_id.as_str())))?;
        Ok((track.id.to_string(), destination(&track).await?))
    }
    async fn candidates(&self, track_id: &str) -> Result<Vec<Candidate>, RpcError> {
        Ok(candidate_facts(self, track_id)
            .await?
            .into_iter()
            .map(|fact| Candidate {
                done: fact.status == TaskStatus::Done.wire_label(),
                attempt_id: fact.attempt_id,
                commit_sha: fact.commit_sha,
                status: fact.status,
            })
            .collect())
    }
    async fn execute(
        &self,
        track_id: &str,
        card_id: &str,
        worktree: PathBuf,
        payload: PluginForgePayload,
    ) -> Result<PublishOutcome, RpcError> {
        let runtime = self
            .operation_runtime
            .get()
            .cloned()
            .ok_or_else(|| internal("operation runtime not bound"))?;
        let submitted = submit_forge_action_with_key(
            &runtime,
            &self.gate_logs_dir,
            GIT_FORGE_PLUGIN_ID,
            track_id.to_string(),
            card_id.to_string(),
            worktree,
            payload,
            new_id(),
        )
        .await?
        .map_err(|error| {
            RpcError::conflict(format!(
                "refused: publish-key-reused: {error}; call again with a new idempotency_key"
            ))
        })?;
        let result = runtime.wait(&submitted.op_id).await.map_err(internal)?;
        let op_id = result.op_id;
        Ok(match result.outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => PublishOutcome::Succeeded {
                op_id,
                event: result["event"].clone(),
            },
            OperationOutcome::Failed { last_error, .. } => PublishOutcome::Failed {
                op_id,
                reason: last_error,
            },
            OperationOutcome::Stuck { reason, .. } => PublishOutcome::Failed { op_id, reason },
        })
    }
}
fn dev_publish_descriptor() -> ToolDescriptor {
    super::super::descriptor(publish::descriptor())
}
pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(
        dev_publish_descriptor(),
        Arc::new(|ctx, identity, args| {
            Box::pin(async move {
                publish::call(ctx.as_ref(), &identity.card_id, args)
                    .await
                    .map(crate::mcp_server::result::ToolResult::structured)
            })
        }),
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CardRole;
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
}
