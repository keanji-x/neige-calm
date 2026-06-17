//! Runtime lookup helpers.

use std::collections::HashMap;

use crate::db::RouteRepo;
use crate::error::Result;
use crate::event::Event;
use crate::model::{Card, CardRuntimeView};
use crate::runtime_repo::{
    AgentProvider, Result as RuntimeResult, RunStatus, RuntimeKind, RuntimeRepo,
    WorkerSessionProjection,
};
use serde_json::Value;

/// Resolve an active codex thread for a card. Worker sessions are the source of
/// truth.
pub async fn resolve_active_thread_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let Some(runtime) = repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?
    else {
        return Ok(None);
    };
    Ok(non_empty(runtime.thread_id.as_deref()).map(ToOwned::to_owned))
}

/// Resolve the owning card for a provider thread id. Worker sessions are the
/// source of truth.
pub async fn resolve_card_for_thread(
    repo: &dyn RouteRepo,
    provider: AgentProvider,
    thread_id: &str,
) -> Result<Option<String>> {
    let active = match &provider {
        AgentProvider::Codex => {
            repo.runtime_get_active_by_thread(AgentProvider::Codex, thread_id)
                .await?
        }
        AgentProvider::Claude => {
            repo.runtime_get_active_by_session(AgentProvider::Claude, thread_id)
                .await?
        }
    };
    Ok(active.map(|runtime| runtime.card_id))
}

/// Resolve a Claude session for a card. Worker sessions are the source of truth;
/// `cards.payload.claude_session_id` is a transitional fallback for
/// pre-backfill rows and tracked edge cases.
pub async fn resolve_claude_session_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let runtime = repo
        .runtime_get_projectable_for_card(&card_id.to_string())
        .await?;
    if let Some(runtime) = runtime.as_ref()
        && let Some(session_id) = non_empty(runtime.session_id.as_deref())
    {
        return Ok(Some(session_id.to_string()));
    }

    let card = repo.card_get(card_id).await?;
    let legacy_session = card.as_ref().and_then(|card| {
        card.payload
            .get("claude_session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(ToOwned::to_owned)
    });
    tracing::warn!(
        target: "runtime_lookup::fallback",
        card_id,
        runtime_id = runtime.as_ref().map(|runtime| runtime.id.as_str()),
        legacy_hit = legacy_session.is_some(),
        "runtime Claude session lookup missed; falling back to card payload"
    );
    Ok(legacy_session)
}

/// Return active shared codex thread attribution from worker sessions.
pub async fn merge_active_shared_thread_attribution(
    repo: &dyn RouteRepo,
) -> Result<HashMap<String, String>> {
    let runtime_rows = repo.runtime_active_shared_thread_attribution().await?;
    let mut merged = HashMap::new();
    for (thread_id, card_id) in runtime_rows {
        merged.insert(card_id, thread_id);
    }
    Ok(merged)
}

/// Project active runtime identity onto a `Card`'s payload for API/WS compatibility.
///
/// Until the frontend reads directly from runtime fields, the payload must still
/// carry `terminal_id`, `claude_session_id`, `codex_thread_id`, `codex_source`,
/// and `codex_thread_status` for in-flight UI cases. This helper looks up the
/// projectable runtime and patches those keys into payload before serialization.
///
/// Runtime row is SOT; fields the runtime knows are overwritten in payload to
/// reflect runtime truth. Fields the runtime has no opinion on are left
/// untouched.
pub async fn project_runtime_into_card_payload<R: RuntimeRepo + ?Sized>(
    repo: &R,
    card: &mut Card,
) -> RuntimeResult<()> {
    let Some(runtime) = repo
        .runtime_get_projectable_for_card(&card.id.to_string())
        .await?
    else {
        return Ok(());
    };
    project_runtime_fields(card, &runtime);
    Ok(())
}

pub async fn project_runtime_into_cards_payload<R: RuntimeRepo + ?Sized>(
    repo: &R,
    cards: &mut [Card],
) -> RuntimeResult<()> {
    let card_ids = cards
        .iter()
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    let runtimes = repo.runtime_get_projectable_for_cards(&card_ids).await?;
    for card in cards {
        if let Some(runtime) = runtimes.get(&card.id.to_string()) {
            project_runtime_fields(card, runtime);
        }
    }
    Ok(())
}

pub async fn project_runtime_into_event_payload<R: RuntimeRepo + ?Sized>(
    repo: &R,
    event: &mut Event,
) -> RuntimeResult<()> {
    match event {
        Event::CardAdded(card) | Event::CardUpdated(card) => {
            project_runtime_into_card_payload(repo, card).await?;
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn project_runtime_fields(card: &mut Card, runtime: &WorkerSessionProjection) {
    let terminal_id = non_empty(runtime.terminal_run_id.as_deref()).map(ToOwned::to_owned);
    let thread_id = non_empty(runtime.thread_id.as_deref()).map(ToOwned::to_owned);
    let session_id = non_empty(runtime.session_id.as_deref()).map(ToOwned::to_owned);
    let source = (runtime.kind == RuntimeKind::SharedSpec).then(|| "shared".to_string());
    let thread_status = projected_thread_status(runtime).map(ToOwned::to_owned);

    card.runtime = Some(CardRuntimeView {
        runtime_id: runtime.id.clone(),
        kind: runtime.kind.clone(),
        status: runtime.status.clone(),
        provider: runtime.agent_provider.clone(),
        terminal_id: terminal_id.clone(),
        thread_id: thread_id.clone(),
        session_id: session_id.clone(),
        source: source.clone(),
        thread_status: thread_status.clone(),
    });

    let Some(map) = card.payload.as_object_mut() else {
        return;
    };

    if let Some(terminal_id) = terminal_id {
        map.insert("terminal_id".into(), Value::String(terminal_id));
    }

    if runtime.kind == RuntimeKind::ClaudeCard
        && let Some(session_id) = session_id
    {
        map.insert("claude_session_id".into(), Value::String(session_id));
    }

    if matches!(
        runtime.kind,
        RuntimeKind::CodexCard | RuntimeKind::SharedSpec
    ) && let Some(thread_id) = thread_id
    {
        map.insert("codex_thread_id".into(), Value::String(thread_id));
    }

    if let Some(source) = source {
        map.insert("codex_source".into(), Value::String(source));
    }

    if let Some(thread_status) = thread_status {
        map.insert("codex_thread_status".into(), Value::String(thread_status));
    }
}

pub(crate) fn runtime_view_from_runtime(runtime: &WorkerSessionProjection) -> CardRuntimeView {
    CardRuntimeView {
        runtime_id: runtime.id.clone(),
        kind: runtime.kind.clone(),
        status: runtime.status.clone(),
        provider: runtime.agent_provider.clone(),
        terminal_id: non_empty(runtime.terminal_run_id.as_deref()).map(ToOwned::to_owned),
        thread_id: non_empty(runtime.thread_id.as_deref()).map(ToOwned::to_owned),
        session_id: non_empty(runtime.session_id.as_deref()).map(ToOwned::to_owned),
        source: (runtime.kind == RuntimeKind::SharedSpec).then(|| "shared".to_string()),
        thread_status: projected_thread_status(runtime).map(ToOwned::to_owned),
    }
}

fn projected_thread_status(runtime: &WorkerSessionProjection) -> Option<&'static str> {
    if !matches!(
        runtime.kind,
        RuntimeKind::CodexCard | RuntimeKind::SharedSpec
    ) {
        return None;
    }

    match runtime.status {
        RunStatus::TurnPending if non_empty(runtime.thread_id.as_deref()).is_none() => {
            Some("pending_thread_start")
        }
        RunStatus::Failed if non_empty(runtime.thread_id.as_deref()).is_none() => {
            Some("failed_to_spawn")
        }
        RunStatus::Running if non_empty(runtime.thread_id.as_deref()).is_some() => Some("started"),
        _ => None,
    }
}

/// Runtime-first shared-codex discriminator. When no active runtime is
/// available, falls back to the legacy payload stamp.
///
/// Returns true for any active codex card with a thread id; post-PR2a all codex
/// traffic routes through the shared daemon, not only spec-card launches.
pub fn card_is_shared_spec(card: &Card, runtime: Option<&WorkerSessionProjection>) -> bool {
    if let Some(runtime) = runtime {
        return runtime_marks_shared(runtime);
    }

    let legacy_shared = card
        .payload
        .get("codex_source")
        .and_then(serde_json::Value::as_str)
        == Some("shared");
    if legacy_shared {
        tracing::warn!(
            target: "runtime_lookup::fallback",
            card_id = %card.id,
            "runtime shared-card discriminator missed; falling back to card payload"
        );
    }
    legacy_shared
}

fn runtime_marks_shared(runtime: &WorkerSessionProjection) -> bool {
    matches!(runtime.kind, RuntimeKind::SharedSpec)
        || (matches!(runtime.kind, RuntimeKind::CodexCard)
            && runtime.agent_provider == Some(AgentProvider::Codex)
            && runtime
                .thread_id
                .as_deref()
                .is_some_and(|thread_id| !thread_id.trim().is_empty()))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
