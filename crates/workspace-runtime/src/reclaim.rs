//! Shared extra-worktree policy for close ticks and post-delete sweeps.
use crate::{error::WorkspaceError, track_worktree::ObservePoint};
use async_trait::async_trait;
use workspace::reclaim::{ReclaimOutcome, ReclaimRequest};

#[derive(Clone, Debug)]
pub struct ExtraWorktree {
    pub provision_id: i64,
    pub track_id: String,
    pub card_id: String,
    pub area_id: String,
    pub request: ReclaimRequest,
}

#[async_trait]
pub trait ReclaimReceipts: Send + Sync {
    type Error: std::fmt::Display + Send;
    async fn refresh(&self, target: &ExtraWorktree) -> Result<Option<ReclaimRequest>, Self::Error>;
    async fn removed(&self, target: &ExtraWorktree) -> Result<(), Self::Error>;
}

/// Successful filesystem effects alone produce removed receipts. Failures retain the event claim
/// and warn; close ticks can retry, independent of the ignored-clean history.
pub async fn reclaim_extra_worktrees<P: ReclaimReceipts>(
    targets: Vec<ExtraWorktree>,
    ports: &P,
    observe: ObservePoint,
) -> Result<(), WorkspaceError> {
    for target in targets {
        let request = match ports.refresh(&target).await {
            Ok(Some(request)) => request,
            Ok(None) => {
                tracing::warn!(path=%target.request.path.display(), "extra worktree retained: receipt changed");
                continue;
            }
            Err(error) => {
                tracing::warn!(path=%target.request.path.display(), %error, "extra worktree retained: facts unavailable");
                continue;
            }
        };
        let outcome =
            tokio::task::spawn_blocking(move || workspace::reclaim::reclaim(&request, observe))
                .await
                .map_err(|e| WorkspaceError::Internal(format!("extra reclaim task: {e}")))?;
        match outcome {
            Ok(ReclaimOutcome::Removed) => {
                if let Err(error) = ports.removed(&target).await {
                    tracing::warn!(path = %target.request.path.display(), %error, "extra worktree removed receipt failed");
                }
            }
            Ok(ReclaimOutcome::Retained(reason)) => {
                tracing::warn!(path = %target.request.path.display(), reason, "extra worktree retained");
            }
            Err(error) => {
                tracing::warn!(path = %target.request.path.display(), %error, "extra worktree retained")
            }
        }
    }
    Ok(())
}
