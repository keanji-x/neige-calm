//! Candidate construction shared by boot, lazy replacement and explicit activation.
//! Callers own quiescence/reservation and must CAS the returned complete source before install.
use std::sync::Arc;

use serde_json::Value;

use super::profile::{HarnessProfile, PlannerBinding};
use super::{HarnessSnapshot, replay_harness_events_since};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::ids::{CardId, TrackId};
use crate::session_projection_repo::{AgentProvider, WorkerSessionProjection};

pub(crate) async fn prepare_quiesced(
    repo: Arc<dyn Repo>,
    runtime: &WorkerSessionProjection,
    track: &TrackId,
    binding: &PlannerBinding,
) -> Result<(Value, HarnessSnapshot)> {
    let source = runtime
        .handle_state_json
        .clone()
        .ok_or_else(|| CalmError::Conflict("harness recovery checkpoint missing".into()))?;
    let mut snapshot = HarnessSnapshot::parse_known(source.clone())
        .ok_or_else(|| CalmError::Conflict("invalid harness recovery snapshot".into()))?;
    if let Some(receipt) = snapshot.handoff_pause.receipt()
        && !receipt.matches_binding(&CardId::from(runtime.card_id.clone()), &runtime.id, track)
    {
        return Err(CalmError::Conflict(
            "handoff pause recovery binding mismatch".into(),
        ));
    }
    if binding.provider == AgentProvider::OpenCode {
        crate::acp_planner::recovery::recover(
            repo.as_ref(),
            &runtime.id,
            &runtime.card_id,
            track.as_str(),
            &mut snapshot,
        )
        .await?;
    }
    snapshot.handoff_pause.unconfirm_recovered_health();
    if binding.profile == HarnessProfile::Planner {
        let watermark = snapshot.push_watermark;
        replay_harness_events_since(repo, &runtime.card_id, track, watermark, &mut snapshot)
            .await?;
    }
    Ok((source, snapshot))
}
