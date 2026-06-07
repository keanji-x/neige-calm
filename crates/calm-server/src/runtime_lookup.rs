//! Runtime-first lookup helpers with legacy fallback during the PR2b read switch.

use std::collections::HashMap;

use crate::db::RouteRepo;
use crate::error::Result;
use crate::model::Card;
use crate::runtime_repo::{AgentProvider, CardRuntime, RuntimeKind};

/// Resolve an active codex thread for a card. Runtime rows are the source of
/// truth; `card_codex_threads` is a transitional fallback for pre-backfill
/// rows and tracked edge cases.
pub async fn resolve_active_thread_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let active = repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?;
    if let Some(runtime) = active.as_ref()
        && let Some(thread_id) = non_empty(runtime.thread_id.as_deref())
    {
        return Ok(Some(thread_id.to_string()));
    }

    let legacy = repo.card_codex_thread_get_by_card(card_id).await?;
    tracing::warn!(
        target: "runtime_lookup::fallback",
        card_id,
        runtime_id = active.as_ref().map(|runtime| runtime.id.as_str()),
        legacy_hit = legacy.is_some(),
        "runtime card->thread lookup missed; falling back to card_codex_threads"
    );
    Ok(legacy.map(|row| row.thread_id))
}

/// Resolve the owning card for a provider thread id. Runtime rows are the
/// source of truth; `card_codex_threads` is a transitional fallback for
/// pre-backfill rows and tracked edge cases.
pub async fn resolve_card_for_thread(
    repo: &dyn RouteRepo,
    provider: AgentProvider,
    thread_id: &str,
) -> Result<Option<String>> {
    let active = repo
        .runtime_get_active_by_thread(provider.clone(), thread_id)
        .await?;
    if let Some(runtime) = active.as_ref() {
        return Ok(Some(runtime.card_id.clone()));
    }

    let legacy = repo.card_codex_thread_get_by_thread(thread_id).await?;
    tracing::warn!(
        target: "runtime_lookup::fallback",
        thread_id,
        provider = ?provider,
        legacy_hit = legacy.is_some(),
        "runtime thread->card lookup missed; falling back to card_codex_threads"
    );
    Ok(legacy.map(|row| row.card_id))
}

/// Resolve a Claude session for a card. Runtime rows are the source of truth;
/// `cards.payload.claude_session_id` is a transitional fallback for
/// pre-backfill rows and tracked edge cases.
pub async fn resolve_claude_session_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let active = repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?;
    if let Some(runtime) = active.as_ref()
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
        runtime_id = active.as_ref().map(|runtime| runtime.id.as_str()),
        legacy_hit = legacy_session.is_some(),
        "runtime Claude session lookup missed; falling back to card payload"
    );
    Ok(legacy_session)
}

/// Merge active shared codex thread attribution from runtime rows and legacy
/// `card_codex_threads` rows. Runtime rows are inserted first; legacy rows then
/// overwrite by card id because the daemon-touched legacy row is the most recent
/// active-use signal during the PR2b two-write reset window.
pub async fn merge_active_shared_thread_attribution(
    repo: &dyn RouteRepo,
) -> Result<HashMap<String, String>> {
    let runtime_rows = repo.runtime_active_shared_thread_attribution().await?;
    let legacy_rows = repo.card_codex_threads_active_shared_only().await?;

    let mut merged = HashMap::new();
    for (thread_id, card_id) in runtime_rows {
        merged.insert(card_id, thread_id);
    }

    let mut legacy_fallbacks = 0usize;
    for row in legacy_rows {
        match merged.get(&row.card_id) {
            Some(runtime_thread) if runtime_thread != &row.thread_id => {
                tracing::warn!(
                    target = "runtime_lookup::merge_conflict",
                    card_id = %row.card_id,
                    runtime_thread = %runtime_thread,
                    legacy_thread = %row.thread_id,
                    "runtime and legacy shared thread attribution disagree; using legacy"
                );
            }
            None => legacy_fallbacks += 1,
            _ => {}
        }
        merged.insert(row.card_id, row.thread_id);
    }
    if legacy_fallbacks > 0 {
        tracing::warn!(
            target: "runtime_lookup::fallback",
            count = legacy_fallbacks,
            "runtime shared thread attribution missed rows; merged legacy card_codex_threads fallback"
        );
    }

    Ok(merged)
}

/// Runtime-first shared-codex discriminator. When no active runtime is
/// available, falls back to the legacy payload stamp.
///
/// Returns true for any active codex card with a thread id; post-PR2a all codex
/// traffic routes through the shared daemon, not only spec-card launches.
pub fn card_is_shared_spec(card: &Card, runtime: Option<&CardRuntime>) -> bool {
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

fn runtime_marks_shared(runtime: &CardRuntime) -> bool {
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
