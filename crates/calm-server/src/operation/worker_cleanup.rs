use crate::card_role_cache::CardRoleCache;
use crate::db::RouteRepo;
use crate::db::sqlite::card_with_terminal_rollback_tx;
use crate::terminal_renderer::TerminalRendererRegistry;
use crate::terminal_sweeper::{reap_terminal_artifacts_with_renderer, reap_terminal_pid_only};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerCleanupOutcome {
    Deleted,
    Preserved,
}

pub(crate) async fn worker_spawn_failure_preserved(
    repo: &dyn RouteRepo,
    terminal_id: &str,
) -> crate::error::Result<bool> {
    let Some(term) = repo.terminal_get(terminal_id).await? else {
        return Ok(false);
    };
    Ok(term.exit_code.is_some() || term.signal_killed)
}

pub(crate) async fn compensate_worker_rows(
    repo: &dyn RouteRepo,
    terminal_renderer: &TerminalRendererRegistry,
    card_role_cache: &CardRoleCache,
    card_id: &str,
    terminal_id: &str,
) -> WorkerCleanupOutcome {
    let latest = match repo.terminal_get(terminal_id).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::error!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                error = %e,
                "worker compensation: terminal re-fetch failed; skipping reap \
                 (daemon may leak until sweeper next tick)",
            );
            None
        }
    };

    if let Some(term) = latest.as_ref() {
        if term.exit_code.is_some() || term.signal_killed {
            tracing::error!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?term.exit_code,
                signal_killed = term.signal_killed,
                "worker compensation: reached preserved branch after spawn failure; \
                 this should be unreachable because adapters convert fast-exit evidence to success",
            );
            return WorkerCleanupOutcome::Preserved;
        }

        if terminal_renderer.get(&term.id).is_some() {
            reap_terminal_artifacts_with_renderer(Some(terminal_renderer), term).await;
        } else if let Some(pid) = term.pid {
            reap_terminal_pid_only(&term.id, pid);
        }
    } else {
        tracing::debug!(
            card_id = %card_id,
            terminal_id = %terminal_id,
            "worker compensation: terminal row vanished pre-reap; skipping reap step",
        );
    }

    let card_id_for_tx = card_id.to_string();
    let term_id_for_tx = terminal_id.to_string();
    let cache_for_tx = card_role_cache.clone();
    let rollback = repo
        .write_in_tx(Box::new(move |tx| {
            Box::pin(async move {
                card_with_terminal_rollback_tx(tx, &card_id_for_tx, &term_id_for_tx, &cache_for_tx)
                    .await
            })
        }))
        .await;
    if let Err(e) = rollback {
        tracing::error!(
            card_id = %card_id,
            terminal_id = %terminal_id,
            error = %e,
            "worker compensation rollback failed; sweeper fallback will reap on next tick",
        );
    }
    WorkerCleanupOutcome::Deleted
}

/// A failed start is not permission to discard an execution that may be live.
/// The prepared Operation is the durable obligation, including after abort.
pub(crate) fn may_have_started(op: &super::Operation) -> crate::error::Result<bool> {
    use super::{CompensationStateVersioned, Phase, PhaseTag};
    let phase = match &op.phase {
        Phase::Compensating | Phase::Failed | Phase::Stuck { .. } => {
            let state: CompensationStateVersioned =
                serde_json::from_value(op.compensation_state.clone().ok_or_else(|| {
                    crate::error::CalmError::Conflict(
                        "worker cleanup source phase is missing; retain prepared resources".into(),
                    )
                })?)?;
            if state.version != 1 {
                return Err(crate::error::CalmError::Conflict(
                    "unknown worker compensation version; retain prepared resources".into(),
                ));
            }
            state.from_phase
        }
        phase => phase.tag(),
    };
    Ok(!matches!(
        phase,
        PhaseTag::Pending | PhaseTag::TxCommitted | PhaseTag::AppServerInteract
    ))
}

pub(crate) async fn require_cleanup_safe(
    ctx: &super::SpawnCtx,
    op: &super::Operation,
    output: &super::TxOutput,
    business_may_be_live: bool,
) -> crate::error::Result<()> {
    use super::terminal_launch::RequestState;
    if !may_have_started(op)? {
        return Ok(());
    }
    let state = RequestState::read(&output.data)?;
    if !business_may_be_live && matches!(state, Some(RequestState::NotRequested { .. })) {
        return Ok(());
    }
    let terminal_id = output.output_string("terminal_id", "worker cleanup")?;
    let sock = match state {
        Some(RequestState::Requested {
            terminal_id: recorded,
            supervisor_sock,
            ..
        }) => {
            if recorded != terminal_id {
                return Err(crate::error::CalmError::Conflict(
                    "worker cleanup terminal identity mismatch; retain resources".into(),
                ));
            }
            Some(supervisor_sock)
        }
        _ => ctx
            .terminal_renderer
            .get(&terminal_id)
            .map(|entry| entry.config().supervisor_sock.clone())
            .or_else(|| ctx.daemon.proc_supervisor_sock.clone()),
    };
    if let Some(sock) = sock {
        crate::terminal_renderer::request_terminal_stop(&sock, &terminal_id).await;
    }
    Err(crate::error::CalmError::Conflict(format!(
        "worker launch cleanup is unverified for operation {} terminal {}; prepared rows and workspace retained for reconciliation",
        op.id, terminal_id,
    )))
}
