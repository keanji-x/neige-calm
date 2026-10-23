//! Claude credential revocation and provider-owned Stop/settlement.
//! Boot also retains proof of the pre-C4 marker sweep for processes from older kernels.

use super::config::ClaudePlannerHost;
use super::stop::{SeamPolicy, sweep};
use crate::db::sqlite::{ClaudePlannerScope, claude_planner_revoke_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::Result;

async fn revoke(
    repo: &dyn RepoEventWrite,
    scope: ClaudePlannerScope<'static>,
) -> Result<Vec<String>> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { Ok(claude_planner_revoke_tx(tx, scope).await?) })
    })
    .await
}

/// Before the listener starts, revoke every Claude credential (failure aborts boot),
/// then reconcile registered executions and sweep pre-C4 markers. A failed marker
/// sweep leaves every existing session unproven for this boot. Instructions purge
/// failures only log; the purge is not settlement evidence.
pub async fn boot(repo: &dyn RepoEventWrite, host: &ClaudePlannerHost) -> Result<()> {
    let ids = revoke(repo, ClaudePlannerScope::All).await?;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        "c4-boot-after-revoke",
        &host.instructions_dir.display().to_string(),
    )
    .await;
    let pass_a_ids = ids.clone();
    if let Err(error) = write_in_tx_typed(repo, move |tx| {
        Box::pin(
            async move { crate::provider_execution::sweep::boot_pass_a(tx, &pass_a_ids).await },
        )
    })
    .await
    {
        tracing::warn!(%error, "Claude provider boot pass A failed; durable sweep will retry");
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        "c4-boot-after-pass-a",
        &host.instructions_dir.display().to_string(),
    )
    .await;
    let markers: Vec<&str> = ids.iter().map(String::as_str).collect();
    let unproven = match sweep(&host.instance, &markers, SeamPolicy::Ignore).await {
        Ok(()) => Default::default(),
        Err(error) => {
            tracing::warn!(%error, "claude planner boot pre-C4 sweep did not confirm");
            ids.into_iter().collect()
        }
    };
    host.pre_c4
        .set(super::config::PreC4Sweep { unproven })
        .map_err(|_| {
            crate::error::CalmError::Conflict("Claude boot sweep proof already initialized".into())
        })?;
    match std::fs::read_dir(&host.instructions_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Err(error) = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    std::fs::remove_dir_all(entry.path())
                } else {
                    std::fs::remove_file(entry.path())
                } {
                    tracing::warn!(
                        path = %entry.path().display(),
                        %error,
                        "claude planner boot: could not remove a leftover instructions file"
                    );
                }
            }
        }
        Err(error) => tracing::warn!(
            dir = %host.instructions_dir.display(),
            %error,
            "claude planner boot: could not list the instructions directory"
        ),
    }
    Ok(())
}

/// Before a destructive step on a track (its deletion, its area's deletion, a workspace repoint):
/// revoke then Stop and settle every Claude provider execution of the track, in any state. `Err` aborts the step
/// before anything moves.
pub async fn sweep_track(
    repo: &dyn RepoEventWrite,
    host: &ClaudePlannerHost,
    track_id: &str,
) -> Result<()> {
    sweep_scope(repo, host, ClaudePlannerScope::Track(track_id)).await
}

/// Card deletion uses the same owner sweep with a narrower scope.
pub(crate) async fn sweep_card(
    repo: &dyn RepoEventWrite,
    host: &ClaudePlannerHost,
    card: &str,
) -> Result<()> {
    sweep_scope(repo, host, ClaudePlannerScope::Card(card)).await
}
async fn sweep_scope(
    repo: &dyn RepoEventWrite,
    host: &ClaudePlannerHost,
    scope: ClaudePlannerScope<'_>,
) -> Result<()> {
    enum Scope {
        All,
        Track(String),
        Card(String),
    }
    let scope = match scope {
        ClaudePlannerScope::All => Scope::All,
        ClaudePlannerScope::Track(id) => Scope::Track(id.into()),
        ClaudePlannerScope::Card(id) => Scope::Card(id.into()),
    };
    let sessions = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let scope = match &scope {
                Scope::All => ClaudePlannerScope::All,
                Scope::Track(id) => ClaudePlannerScope::Track(id),
                Scope::Card(id) => ClaudePlannerScope::Card(id),
            };
            let ids = claude_planner_revoke_tx(tx, scope).await?;
            let mut retained = Vec::new();
            for id in ids {
                if calm_truth::db::sqlite::provider_execution::retained(tx, &id).await? {
                    retained.push(id);
                }
            }
            Ok(retained)
        })
    })
    .await?;
    let executions = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut executions = Vec::new();
            for session in sessions {
                if let Some(claim) =
                    calm_truth::db::sqlite::provider_execution::load(&mut **tx, &session).await?
                    && let Some(id) = crate::provider_execution::request_stop_tx(
                        tx,
                        &session,
                        &claim,
                        crate::provider_execution::StopReason::TrackRetired,
                    )
                    .await?
                {
                    executions.push(id);
                }
            }
            Ok(executions)
        })
    })
    .await?;
    if executions.is_empty() {
        return Ok(());
    }
    let engine = crate::provider_execution::engine(host.execution.engine())?;
    crate::execution_quiesce::drive(Some(engine), executions.iter().cloned());
    tokio::time::timeout(workspace_runtime::execution::QUIESCE_DEADLINE, async {
        for id in executions {
            crate::provider_execution::wait_settled(
                engine,
                &id,
                workspace_runtime::execution::QUIESCE_DEADLINE,
            )
            .await?;
        }
        Ok::<_, crate::error::CalmError>(())
    })
    .await
    .map_err(crate::provider_execution::conflict)??;
    Ok(())
}

/// Operation shutdown's durable provider cleanup, including registry-miss replay.
/// Failure remains log-only; strict deletion uses the session's separate shutdown result.
pub(crate) async fn shutdown_replay(host: &ClaudePlannerHost, worker_session_id: &str) {
    if let Err(error) = stop_session(
        host,
        worker_session_id,
        crate::provider_execution::StopReason::Shutdown,
    )
    .await
    {
        tracing::warn!(%worker_session_id, %error, "Claude shutdown retained its execution claim");
    }
}

/// Durable Stop followed by owner completion; never a marker/PID inference.
pub(crate) async fn stop_session(
    host: &ClaudePlannerHost,
    session: &str,
    reason: crate::provider_execution::StopReason,
) -> Result<()> {
    use crate::provider_execution::{self as execution, execution_id};
    let repo = host.completion.repo()?;
    let pool = repo
        .sqlite_db()
        .ok_or(crate::error::CalmError::ExecutionUnavailable)?;
    let Some(claim) =
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), session).await?
    else {
        return Ok(());
    };
    let owner = &host.provider_execution;
    let engine = host.execution.engine();
    let id = execution_id(claim.record())?;
    owner.stop(&pool, engine, session, &claim, reason).await?;
    let engine = execution::engine(engine)?;
    execution::wait_settled(engine, &id, super::session::SETTLE_AFTER_STOP).await?;
    if !owner.drivers.contains(&id) {
        owner
            .finish(&pool, Some(engine), session, &claim, None)
            .await?;
    }
    Ok(())
}

/// R2 preflight outside SQL. A Live handle supplies the old session's issue lock;
/// without a Live handle there is no issuer, and the registered owner reconciles it.
pub(crate) async fn transition_gate(
    repo: &dyn crate::db::Repo,
    host: &ClaudePlannerHost,
    registry: &crate::harness::HarnessRegistry,
    session: &String,
) -> Result<()> {
    if let Some(handle) = registry.get(session) {
        return handle.provider_transition_gate().await;
    }
    let pool = repo
        .sqlite_db()
        .ok_or(crate::error::CalmError::ExecutionUnavailable)?;
    host.provider_execution
        .admission_gate(&pool, host.execution.engine(), session)
        .await
}

/// A reset stops the predecessor by id even on registry-miss replay. Failure is
/// warning-only: boot or the next destructive step retries the scoped sweep.
pub(crate) async fn stop_historical_predecessor(
    host: &ClaudePlannerHost,
    card_id: &str,
    old_worker_session_id: &str,
) {
    if let Err(error) = stop_session(
        host,
        old_worker_session_id,
        crate::provider_execution::StopReason::Shutdown,
    )
    .await
    {
        tracing::warn!(
            card_id, old_worker_session_id, %error,
            "planner harness reset: the Claude Planner predecessor's stop did not confirm"
        );
    }
}
