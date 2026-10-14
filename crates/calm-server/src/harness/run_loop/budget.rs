//! Per-start sampling and a bounded, identity-fenced warning. The watchdog never awaits steer.
use super::{Inner, runtime_is_still_the_live_carrier, write_projection_row};
use crate::harness::profile::HarnessProfile;
use crate::harness::state::HarnessState;
use crate::model::{HarnessInputPresentation, HarnessInputSegment};
pub use crate::planner_turn_budget::FrozenBudget;
use crate::planner_turn_budget::PlannerTurnBudget;
use std::sync::Arc;
use std::time::Duration;

pub(super) async fn sample(inner: &Inner) -> crate::error::Result<Option<PlannerTurnBudget>> {
    let Some(card) = inner.repo.card_get(inner.card_id.as_str()).await? else {
        return Err(crate::error::CalmError::Conflict(
            "budget card disappeared".into(),
        ));
    };
    let role = inner.repo.card_role_get(inner.card_id.as_str()).await?;
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
        PlannerTurnBudget::load(inner.repo.as_ref()).await.map(Some)
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
    if !inner.backend.supports_steer() || !inner.held_requests.is_empty() {
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
        *warning_attempted = true;
        (*budget, start_attempt.clone())
    };
    // Record the one attempt before launching it; failure does not retry or wake another turn.
    super::persist_snapshot(inner).await?;
    let Some(thread) = inner.thread_id.read().await.clone() else {
        return Ok(());
    };
    let key = format!("{}:budget-warning", attempt.as_str());
    let segments = vec![segment(format!(
        "[Trusted Planner turn budget reminder]\nAt most {} seconds of execution remain. Wrap up now before the hard stop.",
        budget.warning_remaining().as_secs()
    ))];
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
            let input = vec![super::InputItem::text(segments[0].text.clone())];
            let send = inner.backend.turn_steer(&thread, &turn_id, input, &key);
            tokio::pin!(send);
            let sent = loop {
                tokio::select! {
                    result = &mut send => break result,
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {
                        if !current(&inner, &turn_id).await { return false; }
                    }
                }
            };
            matches!(sent, Ok(ref accepted) if accepted == &turn_id)
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
