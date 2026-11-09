//! Close uses Planner identity and provider Stop ownership, never Worker absence conventions.
use super::*;
use crate::harness::HarnessPhaseTag;
use crate::session_projection_repo::WorkerSessionProjection;

pub(crate) struct ClosedNativeTarget {
    pub thread: String,
    turn: Option<String>,
}

impl PlannerFactory<'_> {
    /// SQL-only admission, also repeated on the exact persisted cleanup row.
    pub(crate) fn require_closed_planner_binding(runtime: &WorkerSessionProjection) -> Result<()> {
        match runtime.agent_provider.as_ref().map(AgentProvider::protocol) {
            Some(AgentProtocol::Codex) => Self::closed_native_target(runtime).map(|_| ()),
            Some(AgentProtocol::Claude | AgentProtocol::Acp) => Ok(()),
            None => Err(CalmError::Conflict(
                "closed Planner has no provider binding".into(),
            )),
        }
    }

    /// The original input owner identifies a current canonical execution. A
    /// prior header turn cannot address a new uncertain crossing. A moved
    /// predecessor retains only its persisted Stop binding, never successor END.
    pub(crate) async fn require_closed_planner_binding_tx(
        tx: &mut crate::operation::Tx<'_>,
        runtime: &WorkerSessionProjection,
    ) -> Result<()> {
        Self::require_closed_planner_binding(runtime)?;
        let active = runtime
            .handle_state_json
            .as_ref()
            .and_then(|value| value.pointer("/canonical_inputs/active"))
            .is_some_and(|active| !active.is_null());
        if runtime.agent_provider.as_ref().map(AgentProvider::protocol)
            != Some(AgentProtocol::Codex)
            || !active
        {
            return Ok(());
        }
        let current: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1 AND session_id=?2)")
                .bind(&runtime.card_id)
                .bind(&runtime.id)
                .fetch_one(&mut **tx)
                .await?;
        if !current {
            return Ok(());
        }
        let carrier = crate::db::sqlite::conversation::current_input_fact_carrier_tx(
            tx,
            &runtime.card_id,
            &runtime.id,
        )
        .await?;
        let original =
            crate::db::sqlite::conversation::original_input_target_tx(tx, &carrier).await?;
        let scope = original.and_then(|original| {
            crate::harness::backend::ExecutionEndScope::from_correlation(&original.correlation)
        });
        let target = Self::closed_native_target(runtime)?.ok_or_else(|| {
            CalmError::Conflict("closed Planner execution has no Stop target".into())
        })?;
        match scope {
            Some(crate::harness::backend::ExecutionEndScope::NativeTurn {
                thread_id,
                native_turn_id,
            }) if thread_id == target.thread
                && Some(native_turn_id.as_str()) == target.turn.as_deref() =>
            {
                Ok(())
            }
            _ => Err(CalmError::Conflict(
                "closed Planner original execution binding unavailable".into(),
            )),
        }
    }

    /// None is positive evidence of a canonical carrier that never issued native work.
    /// Missing identity on an attempted or historical carrier is a refusal, not absence.
    pub(crate) fn closed_native_target(
        runtime: &WorkerSessionProjection,
    ) -> Result<Option<ClosedNativeTarget>> {
        let snapshot = runtime.handle_state_json.as_ref();
        let canonical = snapshot
            .filter(|value| value.get("canonical_inputs").is_some())
            .map(|value| {
                serde_json::from_value::<crate::harness::snapshot::CanonicalHarnessSnapshot>(
                    value.clone(),
                )
            })
            .transpose()?;
        let Some(thread) = crate::harness::effective_runtime_thread_id(runtime) else {
            if runtime.status.is_active_authority()
                && canonical.as_ref().is_some_and(|frame| {
                    matches!(
                        frame.header.phase,
                        HarnessPhaseTag::Idle | HarnessPhaseTag::PendingThreadStart
                    ) && frame.header.last_thread_id.is_none()
                        && frame.header.last_turn_id.is_none()
                        && frame.header.interruption_intent.is_none()
                        && frame.header.wedged_reason.is_none()
                        && frame.header.pending_rewind.is_none()
                        && frame.header.compact_outcome.is_none()
                        && frame.canonical_inputs.active.is_none()
                        && frame.canonical_inputs.attempts.is_empty()
                })
            {
                return Ok(None);
            }
            return Err(CalmError::Conflict(
                "closed Planner stop identity unavailable".into(),
            ));
        };
        let recorded = snapshot
            .and_then(|value| value.get("last_thread_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty());
        if recorded.is_some_and(|recorded| recorded != thread)
            || canonical.as_ref().is_some_and(|frame| {
                frame.canonical_inputs.attempts.iter().any(|attempt| {
                    attempt.correlation.provider != AgentProvider::Codex
                        || attempt.correlation.thread_id != thread
                })
            })
        {
            return Err(CalmError::Conflict(
                "closed Planner thread binding changed".into(),
            ));
        }
        let turn = crate::operation::TxOutput::non_empty_string(runtime.active_turn_id.as_deref())
            .or_else(|| {
                snapshot
                    .and_then(|value| value.get("last_turn_id"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
            });
        if canonical
            .as_ref()
            .is_some_and(|frame| frame.canonical_inputs.active.is_some())
            && turn.is_none()
        {
            return Err(CalmError::Conflict(
                "closed Planner execution has no Stop target".into(),
            ));
        }
        Ok(Some(ClosedNativeTarget { thread, turn }))
    }

    /// Registry-miss cleanup stops only the recorded session; it opens no control or process.
    pub(crate) async fn shutdown_closed_session(
        &self,
        runtime: &WorkerSessionProjection,
        native: Option<&crate::shared_codex_appserver::SharedCodexAppServer>,
    ) -> Result<()> {
        Self::require_closed_planner_binding(runtime)?;
        match runtime.agent_provider.as_ref().map(AgentProvider::protocol) {
            Some(AgentProtocol::Codex) => {
                let Some(target) = Self::closed_native_target(runtime)? else {
                    return Ok(());
                };
                let native = native.ok_or(CalmError::ExecutionUnavailable)?;
                if native
                    .cached_card_for_thread(&target.thread)
                    .is_some_and(|card| card != runtime.card_id)
                    || target
                        .turn
                        .as_deref()
                        .zip(native.active_turn_id_for_thread(&target.thread))
                        .is_some_and(|(expected, actual)| expected != actual)
                {
                    return Err(CalmError::Conflict(
                        "closed Planner native owner changed".into(),
                    ));
                }
                Self::stop_bound_native_thread(native, &target.thread, target.turn.as_deref()).await
            }
            Some(AgentProtocol::Claude) => {
                crate::claude_planner::lifecycle::stop_session(
                    self.claude,
                    &runtime.id,
                    crate::provider_execution::StopReason::Shutdown,
                )
                .await
            }
            Some(AgentProtocol::Acp) => {
                crate::acp_planner::lifecycle::stop_session(
                    self.acp,
                    &runtime.id,
                    crate::provider_execution::StopReason::Shutdown,
                )
                .await
            }
            None => Err(CalmError::Conflict(
                "closed Planner has no provider binding".into(),
            )),
        }
    }
}
