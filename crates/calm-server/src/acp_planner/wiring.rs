use super::{
    config::AcpPlannerHost,
    session::{AcpPlannerSession, SessionParams},
};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::wiring::{PlannerRow, PlannerWiring};
use crate::thread_seals::ThreadSeals;
use std::sync::Arc;

pub(crate) async fn open_session(
    host: &Arc<AcpPlannerHost>,
    wiring: &PlannerWiring,
    repo: Arc<dyn Repo>,
    seals: Arc<ThreadSeals>,
    row: PlannerRow<'_>,
) -> Result<Arc<AcpPlannerSession>> {
    let track = repo
        .track_get(row.track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("ACP track".into()))?;
    let instructions = crate::harness::instructions::profile_instructions(
        repo.as_ref(),
        &wiring.plugin,
        row.track_id,
        row.card_id,
        &row.binding,
    )
    .await?;
    Ok(Arc::new(AcpPlannerSession::open(SessionParams {
        engine: host.execution.engine().cloned(),
        host: host.clone(),
        provider: row.binding.provider,
        repo,
        seals,
        worker_session_id: row.worker_session_id.into(),
        card_id: row.card_id.into(),
        track_id: row.track_id.into(),
        cwd: track.workspace.agent_cwd().into(),
        instructions,
    })))
}

/// Read-only native maintenance declaration; profile policy stays private.
pub(crate) fn manual_compaction(provider: &calm_types::runtime::AgentProvider) -> bool {
    super::profile::AcpAgentProfile::of(provider)
        .is_some_and(|profile| profile.compaction.is_some())
}

/// Canonical support is registration policy owned by the ACP profile and contract.
pub(crate) fn canonical_inputs(
    provider: &calm_types::runtime::AgentProvider,
    contract: calm_types::worker::WorkerContract,
) -> bool {
    super::profile::AcpAgentProfile::of(provider)
        .is_some_and(|profile| profile.canonical_inputs(contract))
}
