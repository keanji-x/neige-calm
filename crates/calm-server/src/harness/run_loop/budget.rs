//! Per-start sampling and a bounded, identity-fenced warning. The watchdog never awaits steer.
use super::{Inner, runtime_is_still_the_live_carrier, write_projection_row};
use crate::harness::profile::{HarnessProfile, HarnessProfileExt};
use crate::harness::state::HarnessState;
use crate::model::{HarnessInputPresentation, HarnessInputSegment};
pub use crate::planner_turn_budget::FrozenBudget;
use crate::planner_turn_budget::PlannerTurnBudgetExt;
use crate::planner_turn_budget::{LoadError, PlannerTurnBudget};
use std::sync::Arc;
use std::time::Duration;

pub(super) async fn sample(
    inner: &Inner,
) -> Result<Option<PlannerTurnBudget>, super::IssuanceRefusal> {
    let Some(card) = inner
        .repo
        .card_get(inner.card_id.as_str())
        .await
        .map_err(|error| read_refusal(error.into()))?
    else {
        return Err(read_refusal(crate::error::CalmError::Conflict(
            "budget card disappeared".into(),
        )));
    };
    let role = inner
        .repo
        .card_role_get(inner.card_id.as_str())
        .await
        .map_err(|error| read_refusal(error.into()))?;
    let is_planner = role
        .and_then(|role| HarnessProfile::from_shape(&card.kind, role, &card.payload))
        == Some(HarnessProfile::Planner);
    if is_planner {
        // An interrupted start attempt replays under its frozen budget and stable projection id.
        if let Some(id) = inner.projection_client_id.lock().await.as_ref()
            && let FrozenBudget::Planner {
                budget,
                start_attempt,
                ..
            } = &*inner.turn_budget.lock().await
            && id == start_attempt
        {
            return Ok(Some(*budget));
        }
        PlannerTurnBudget::load(inner.repo.as_ref())
            .await
            .map(Some)
            .map_err(sampling_refusal)
    } else {
        Ok(None)
    }
}

pub(super) fn segment(text: String) -> HarnessInputSegment {
    HarnessInputSegment {
        presentation: HarnessInputPresentation::System,
        text,
        attachments: vec![],
        origin: None,
    }
}

pub(super) async fn limit(inner: &Inner) -> Duration {
    match &*inner.turn_budget.lock().await {
        FrozenBudget::Planner { budget, .. } => budget.limit(),
        FrozenBudget::Legacy | FrozenBudget::NotApplicable => inner.config.max_turn_duration,
    }
}

async fn current(inner: &Arc<Inner>, turn: &str) -> bool {
    let state = inner.state.lock().await.clone();
    let turn_limit = limit(inner).await;
    !inner
        .shutting_down
        .load(std::sync::atomic::Ordering::SeqCst)
        && inner.held_requests.is_empty()
        && inner.backend.issuance_hold().is_none()
        && matches!(&state, HarnessState::TurnRunning {turn_id, watchdog_from, ..} if turn_id == turn && watchdog_from.elapsed() < turn_limit)
        && runtime_is_still_the_live_carrier(inner)
            .await
            .unwrap_or(false)
}

pub(super) async fn warn(inner: &Arc<Inner>) -> crate::error::Result<()> {
    if !inner.backend.supports_budget_control() || !inner.held_requests.is_empty() {
        return Ok(());
    }
    let state = inner.state.lock().await.clone();
    let HarnessState::TurnRunning {
        turn_id,
        watchdog_from,
        ..
    } = state
    else {
        return Ok(());
    };
    let (budget, attempt) = {
        let mut frozen = inner.turn_budget.lock().await;
        let FrozenBudget::Planner {
            budget,
            start_attempt,
            warning_attempted,
        } = &mut *frozen
        else {
            return Ok(());
        };
        let elapsed = watchdog_from.elapsed();
        if *warning_attempted
            || elapsed < budget.limit() - budget.warning_remaining()
            || elapsed >= budget.limit()
        {
            return Ok(());
        }
        if inner.inputs.canonical().is_none() {
            *warning_attempted = true;
        }
        (*budget, start_attempt.clone())
    };
    if inner.inputs.canonical().is_some() {
        return super::canonical::control::warn(inner, &turn_id, budget, &attempt).await;
    }
    // Record the one attempt before launching it; failure does not retry or wake another turn.
    super::persist_snapshot(inner).await?;
    let Some(thread) = inner.thread_id.read().await.clone() else {
        return Ok(());
    };
    let key = format!("{}:budget-warning", attempt.as_str());
    let notice = crate::planner_turn_budget::BudgetControlNotice::for_turn(
        budget,
        &thread,
        &turn_id,
        attempt.as_str(),
    )
    .map_err(|error| crate::error::CalmError::Conflict(error.into()))?;
    let segments = vec![segment(notice.text())];
    // Project BEFORE the RPC: an echo can arrive while the detached call awaits its answer.
    // The existing echo upgrade retains System presentation and never inserts a second row.
    if !current(inner, &turn_id).await {
        return Ok(());
    }
    write_projection_row(inner, &thread, &key, &segments).await?;
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let work = async {
            if !current(&inner, &turn_id).await {
                return false;
            }
            let admit = || {
                Box::pin(async {
                    if current(&inner, &turn_id).await {
                        Ok(())
                    } else {
                        Err(crate::error::CalmError::Conflict(
                            "budget target changed".into(),
                        ))
                    }
                }) as futures_util::future::BoxFuture<'_, crate::error::Result<()>>
            };
            let send = inner.backend.send_budget_notice(
                &notice,
                crate::harness::backend::BudgetControlScope::Historical,
                &admit,
            );
            tokio::pin!(send);
            let sent = loop {
                tokio::select! {
                    result = &mut send => break result,
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {
                        if !current(&inner, &turn_id).await { return false; }
                    }
                }
            };
            matches!(sent.delivery(), crate::harness::backend::ControlDelivery::Accepted { native_turn_id } if native_turn_id == &turn_id)
                && current(&inner, &turn_id).await
        };
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), work).await,
            Ok(true)
        ) {
            // A missing reply cannot prove the provider did not record the input. Keep the
            // System projection for a late echo; the trusted terminal sweep discards it if
            // still unconfirmed. This does not retry or enqueue another turn.
            tracing::debug!("budget reminder delivery was not confirmed");
        }
    });
    Ok(())
}

async fn remove_projection(inner: &Inner, key: &str) {
    if let Err(error) = inner
        .repo
        .transcript_projection_delete(inner.card_id.as_str(), key)
        .await
    {
        tracing::warn!(%error, "could not remove unrecorded budget reminder projection");
    }
}

/// Completion uses the same projection rule as ordinary steered input. A reminder
/// Codex never recorded is discarded, never re-buffered as a later turn's user message.
pub(super) async fn settle_projection(inner: &Inner) {
    let key = match &*inner.turn_budget.lock().await {
        FrozenBudget::Planner {
            start_attempt,
            warning_attempted: true,
            ..
        } => Some(format!("{}:budget-warning", start_attempt.as_str())),
        _ => None,
    };
    if let Some(key) = key {
        remove_projection(inner, &key).await;
    }
}

fn read_refusal(error: crate::error::CalmError) -> super::IssuanceRefusal {
    super::IssuanceRefusal::retryable(format!("could not sample Planner turn budget: {error}"))
}

fn sampling_refusal(error: LoadError) -> super::IssuanceRefusal {
    match error {
        LoadError::InvalidSetting(error) => super::IssuanceRefusal::rejected(
            format!("Planner turn budget: {error}"),
            format!("Cannot start this turn: {error}. Reset the Planner duration in Settings."),
        ),
        LoadError::Read(error) => read_refusal(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::prelude::*;
    use crate::harness::run_loop::completed_commit_tests::Fixture;
    use crate::planner_model::FailureKind;

    #[tokio::test]
    async fn planner_turn_budget_storage_failure_is_retryable_without_reset_advice() {
        let fx = Fixture::new().await;
        fx.repo.pool().close().await;
        let error = PlannerTurnBudget::load(fx.repo.as_ref()).await.unwrap_err();
        let original = error.to_string();
        let failure = sampling_refusal(error);
        assert_eq!(failure.kind, FailureKind::Retryable);
        assert!(failure.reader.is_empty());
        assert!(failure.log.contains(&original), "{}", failure.log);
    }

    #[tokio::test]
    async fn planner_turn_budget_missing_card_is_retryable_without_reset_advice() {
        let fx = Fixture::new().await;
        sqlx::query("DELETE FROM cards WHERE id=?1")
            .bind(fx.harness.inner.card_id.as_str())
            .execute(fx.repo.pool())
            .await
            .unwrap();
        let failure = sample(&fx.harness.inner).await.unwrap_err();
        assert_eq!(failure.kind, FailureKind::Retryable);
        assert!(failure.reader.is_empty());
    }

    #[tokio::test]
    async fn planner_turn_budget_invalid_setting_is_rejected_with_reset_advice() {
        let fx = Fixture::new().await;
        fx.repo
            .settings_upsert(crate::planner_turn_budget::KEY, "broken")
            .await
            .unwrap();
        let failure =
            sampling_refusal(PlannerTurnBudget::load(fx.repo.as_ref()).await.unwrap_err());
        assert_eq!(failure.kind, FailureKind::Rejected);
        assert!(
            failure
                .reader
                .contains("Reset the Planner duration in Settings")
        );
    }
}
