//! Runtime lookup helpers.

use std::collections::HashMap;

use crate::db::RouteRepo;
use crate::error::Result;
use crate::event::Event;
use crate::model::{Card, CardRuntimeView};
use crate::session_projection_repo::{
    AgentProvider, Result as WorkerSessionProjectionResult, WorkerSessionKind,
    WorkerSessionProjection, WorkerSessionProjectionRepo, WorkerSessionState,
};
use crate::worker::WorkerSessionId;
use serde_json::Value;

pub async fn resolve_active_thread_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let Some(runtime) = repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
    else {
        return Ok(None);
    };
    Ok(non_empty(runtime.thread_id.as_deref()).map(ToOwned::to_owned))
}

pub async fn resolve_card_for_thread(
    repo: &dyn RouteRepo,
    provider: AgentProvider,
    thread_id: &str,
) -> Result<Option<String>> {
    Ok(resolve_session_for_thread(repo, provider, thread_id)
        .await?
        .map(|(_session_id, card_id)| card_id))
}

pub async fn resolve_session_for_thread(
    repo: &dyn RouteRepo,
    provider: AgentProvider,
    thread_id: &str,
) -> Result<Option<(WorkerSessionId, String)>> {
    let active = match &provider {
        AgentProvider::Codex => {
            repo.session_projection_active_by_thread(AgentProvider::Codex, thread_id)
                .await?
        }
        AgentProvider::Claude => {
            repo.session_projection_active_by_session(AgentProvider::Claude, thread_id)
                .await?
        }
        AgentProvider::OpenCode => {
            repo.session_projection_active_by_session(AgentProvider::OpenCode, thread_id)
                .await?
        }
    };
    Ok(active.map(|runtime| (WorkerSessionId::from(runtime.id), runtime.card_id)))
}

/// `cards.payload.claude_session_id` is a transitional fallback for pre-backfill rows.
pub async fn resolve_claude_session_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Option<String>> {
    let runtime = repo
        .session_projection_projectable_for_card(&card_id.to_string())
        .await?;
    if let Some(session_id) = claude_session_of_runtime(runtime.as_ref()) {
        return Ok(Some(session_id));
    }
    let card = repo.card_get(card_id).await?;
    Ok(legacy_claude_session_of_card(
        card_id,
        runtime.as_ref(),
        card.as_ref(),
    ))
}

/// The Claude session the card's projectable runtime carries, if any.
pub fn claude_session_of_runtime(runtime: Option<&WorkerSessionProjection>) -> Option<String> {
    runtime
        .and_then(|runtime| non_empty(runtime.session_id.as_deref()))
        .map(ToOwned::to_owned)
}

/// The transitional fallback when the runtime carries no session: the card payload's
/// `claude_session_id`. Logged, since only a pre-backfill row should reach it.
pub fn legacy_claude_session_of_card(
    card_id: &str,
    runtime: Option<&WorkerSessionProjection>,
    card: Option<&Card>,
) -> Option<String> {
    let legacy_session = card.and_then(|card| {
        card.payload
            .get("claude_session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(ToOwned::to_owned)
    });
    tracing::warn!(
        target: "session_projection_lookup::fallback",
        card_id,
        runtime_id = runtime.as_ref().map(|runtime| runtime.id.as_str()),
        legacy_hit = legacy_session.is_some(),
        "runtime Claude session lookup missed; falling back to card payload"
    );
    legacy_session
}

pub async fn merge_active_shared_thread_attribution(
    repo: &dyn RouteRepo,
) -> Result<HashMap<String, String>> {
    let runtime_rows = repo
        .session_projection_active_shared_thread_attribution()
        .await?;
    let mut merged = HashMap::new();
    for (thread_id, card_id) in runtime_rows {
        merged.insert(card_id, thread_id);
    }
    Ok(merged)
}

/// Patch active runtime identity into a `Card`'s payload for API/WS
/// compatibility. The runtime row is SOT; fields it has no opinion on are
/// left untouched.
pub async fn project_runtime_into_card_payload<R: WorkerSessionProjectionRepo + ?Sized>(
    repo: &R,
    card: &mut Card,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = repo
        .session_projection_projectable_for_card(&card.id.to_string())
        .await?
    else {
        return Ok(());
    };
    project_runtime_fields(card, &runtime);
    Ok(())
}

pub async fn project_runtime_into_cards_payload<R: WorkerSessionProjectionRepo + ?Sized>(
    repo: &R,
    cards: &mut [Card],
) -> WorkerSessionProjectionResult<()> {
    let card_ids = cards
        .iter()
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    let runtimes = repo
        .session_projection_projectable_for_cards(&card_ids)
        .await?;
    for card in cards {
        if let Some(runtime) = runtimes.get(&card.id.to_string()) {
            project_runtime_fields(card, runtime);
        }
    }
    Ok(())
}

pub async fn project_runtime_into_event_payload<R: WorkerSessionProjectionRepo + ?Sized>(
    repo: &R,
    event: &mut Event,
) -> WorkerSessionProjectionResult<()> {
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
    let source = projected_shared_codex_source(runtime).map(ToOwned::to_owned);
    let thread_status = projected_thread_status(runtime).map(ToOwned::to_owned);

    card.runtime = Some(runtime_view_from_runtime(runtime));

    let Some(map) = card.payload.as_object_mut() else {
        return;
    };

    if let Some(terminal_id) = terminal_id {
        map.insert("terminal_id".into(), Value::String(terminal_id));
    }

    if runtime.kind == WorkerSessionKind::ClaudeCard
        && let Some(session_id) = session_id
    {
        map.insert("claude_session_id".into(), Value::String(session_id));
    }

    if runtime_uses_codex(runtime)
        && let Some(thread_id) = thread_id
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
        worker_session_id: runtime.id.clone(),
        kind: runtime.kind.clone(),
        status: runtime.status,
        updated_at_ms: Some(runtime.updated_at_ms),
        provider: runtime.agent_provider.clone(),
        terminal_id: non_empty(runtime.terminal_run_id.as_deref()).map(ToOwned::to_owned),
        thread_id: non_empty(runtime.thread_id.as_deref()).map(ToOwned::to_owned),
        session_id: non_empty(runtime.session_id.as_deref()).map(ToOwned::to_owned),
        source: projected_shared_codex_source(runtime).map(ToOwned::to_owned),
        thread_status: projected_thread_status(runtime).map(ToOwned::to_owned),
        last_turn_completed_ms: runtime.last_turn_completed_ms,
        claude_version: claude_running_version(runtime).map(ToOwned::to_owned),
    }
}

/// The key of a Claude card runtime's `handle_state_json` that holds the first token
/// `claude --version` printed when its child was started (#2516). A Claude card runtime carries no
/// harness snapshot; its document may also hold the scheduler's `timeout_cleanup` marker (a task
/// worker's), which the version write keeps.
pub const CLAUDE_VERSION_STATE_KEY: &str = "claude_version";

/// The Claude version a Claude card runtime's child was started on, when one was recorded.
pub fn claude_running_version(runtime: &WorkerSessionProjection) -> Option<&str> {
    if runtime.kind != WorkerSessionKind::ClaudeCard {
        return None;
    }
    runtime
        .handle_state_json
        .as_ref()?
        .get(CLAUDE_VERSION_STATE_KEY)
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
}

fn runtime_uses_codex(runtime: &WorkerSessionProjection) -> bool {
    runtime.agent_provider == Some(AgentProvider::Codex)
        && matches!(
            runtime.kind,
            WorkerSessionKind::CodexCard
                | WorkerSessionKind::SharedPlanner
                | WorkerSessionKind::ManagedAssistant
        )
}

fn projected_shared_codex_source(runtime: &WorkerSessionProjection) -> Option<&'static str> {
    (runtime_uses_codex(runtime) && runtime.kind == WorkerSessionKind::SharedPlanner)
        .then_some("shared")
}

fn projected_thread_status(runtime: &WorkerSessionProjection) -> Option<&'static str> {
    if !runtime_uses_codex(runtime) {
        return None;
    }

    match runtime.status {
        WorkerSessionState::TurnPending if non_empty(runtime.thread_id.as_deref()).is_none() => {
            Some("pending_thread_start")
        }
        WorkerSessionState::Failed if non_empty(runtime.thread_id.as_deref()).is_none() => {
            Some("failed_to_spawn")
        }
        WorkerSessionState::Running if non_empty(runtime.thread_id.as_deref()).is_some() => {
            Some("started")
        }
        _ => None,
    }
}

/// Runtime-first shared-codex discriminator; falls back to the legacy payload
/// stamp when no active runtime is available. False excludes the Codex owner;
/// it says nothing about another backend's liveness or teardown obligations.
pub fn card_uses_shared_codex(card: &Card, runtime: Option<&WorkerSessionProjection>) -> bool {
    if let Some(runtime) = runtime {
        return runtime_uses_shared_codex(runtime);
    }

    let legacy_shared = card
        .payload
        .get("codex_source")
        .and_then(serde_json::Value::as_str)
        == Some("shared");
    if legacy_shared {
        tracing::warn!(
            target: "session_projection_lookup::fallback",
            card_id = %card.id,
            "runtime shared-Codex discriminator missed; falling back to card payload"
        );
    }
    legacy_shared
}

fn runtime_uses_shared_codex(runtime: &WorkerSessionProjection) -> bool {
    runtime_uses_codex(runtime)
        && (runtime.kind == WorkerSessionKind::SharedPlanner
            || non_empty(runtime.thread_id.as_deref()).is_some())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod shared_codex_identity_tests {
    use super::*;
    use crate::ids::{CardId, TrackId};
    use serde_json::json;

    fn runtime(
        kind: WorkerSessionKind,
        provider: Option<AgentProvider>,
        thread: Option<&str>,
    ) -> WorkerSessionProjection {
        WorkerSessionProjection {
            id: "worker".into(),
            card_id: "card".into(),
            kind,
            agent_provider: provider,
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: thread.map(str::to_owned),
            session_id: Some("native-session".into()),
            active_turn_id: None,
            handle_state_json: None,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
            last_turn_completed_ms: None,
        }
    }

    fn card(payload: Value) -> Card {
        Card {
            id: CardId::from("card"),
            track_id: TrackId::from("track"),
            kind: "conversation".into(),
            sort: 0.0,
            payload,
            title: None,
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn shared_codex_owner_requires_the_actual_provider() {
        use AgentProvider::{Claude, Codex, OpenCode};
        use WorkerSessionKind::{ClaudeCard, CodexCard, ManagedAssistant, SharedPlanner, Terminal};
        let cases = [
            (SharedPlanner, Some(Codex), None, true),
            (SharedPlanner, Some(Claude), Some("thread"), false),
            (SharedPlanner, Some(OpenCode), Some("thread"), false),
            (SharedPlanner, None, Some("thread"), false),
            (CodexCard, Some(Codex), Some("thread"), true),
            (CodexCard, Some(Codex), Some(" "), false),
            (CodexCard, Some(Codex), None, false),
            (CodexCard, None, Some("thread"), false),
            (ManagedAssistant, Some(Codex), Some("thread"), true),
            (ManagedAssistant, Some(Codex), None, false),
            (ManagedAssistant, Some(Claude), Some("thread"), false),
            (ManagedAssistant, Some(OpenCode), Some("thread"), false),
            (ClaudeCard, Some(Claude), Some("thread"), false),
            (Terminal, None, Some("thread"), false),
        ];
        let card = card(json!({}));
        for (kind, provider, thread, expected) in cases {
            let runtime = runtime(kind, provider, thread);
            assert_eq!(
                card_uses_shared_codex(&card, Some(&runtime)),
                expected,
                "{runtime:?}"
            );
        }
    }

    #[test]
    fn only_codex_planners_project_codex_source_and_thread_status() {
        for (provider, expected_source) in [
            (Some(AgentProvider::Codex), Some("shared")),
            (Some(AgentProvider::Claude), None),
            (Some(AgentProvider::OpenCode), None),
            (None, None),
        ] {
            let runtime = runtime(WorkerSessionKind::SharedPlanner, provider, Some("thread"));
            let mut card = card(json!({}));
            project_runtime_fields(&mut card, &runtime);
            let view = card.runtime.as_ref().unwrap();
            assert_eq!(view.provider, runtime.agent_provider);
            assert_eq!(view.source.as_deref(), expected_source, "{runtime:?}");
            assert_eq!(
                view.thread_status.as_deref(),
                expected_source.map(|_| "started"),
                "{runtime:?}"
            );
            assert_eq!(card.payload["codex_source"].as_str(), expected_source);
            assert_eq!(
                card.payload["codex_thread_id"].as_str(),
                expected_source.map(|_| "thread")
            );
            assert_eq!(
                card.payload["codex_thread_status"].as_str(),
                expected_source.map(|_| "started")
            );
        }
    }

    #[test]
    fn legacy_shared_stamp_applies_only_without_an_active_runtime() {
        let legacy = card(json!({"codex_source":"shared"}));
        assert!(card_uses_shared_codex(&legacy, None));
        assert!(!card_uses_shared_codex(&card(json!({})), None));
        for provider in [
            None,
            Some(AgentProvider::Claude),
            Some(AgentProvider::OpenCode),
        ] {
            let active = runtime(WorkerSessionKind::SharedPlanner, provider, Some("thread"));
            assert!(
                !card_uses_shared_codex(&legacy, Some(&active)),
                "an active runtime must override a stale legacy stamp: {active:?}"
            );
        }
    }
}

#[cfg(test)]
mod claude_version_projection_tests {
    use super::*;
    use crate::ids::{CardId, TrackId};
    use serde_json::json;

    fn runtime(kind: WorkerSessionKind, state: Option<Value>) -> WorkerSessionProjection {
        WorkerSessionProjection {
            id: "ws-1".into(),
            card_id: "card-1".into(),
            kind,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Running,
            terminal_run_id: Some("term-1".into()),
            thread_id: None,
            session_id: Some("session-1".into()),
            active_turn_id: None,
            handle_state_json: state,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
            last_turn_completed_ms: None,
        }
    }

    fn projected(runtime: &WorkerSessionProjection) -> Option<String> {
        let mut card = Card {
            id: CardId::from("card-1"),
            track_id: TrackId::from("track-1"),
            kind: "claude".into(),
            sort: 0.0,
            payload: json!({}),
            title: None,
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        };
        project_runtime_fields(&mut card, runtime);
        card.runtime.expect("projected runtime").claude_version
    }

    /// #2516: a card's projection carries the version its Claude child started on.
    #[test]
    fn a_claude_card_projection_carries_its_running_version() {
        let recorded = Some(json!({ CLAUDE_VERSION_STATE_KEY: "2.1.259" }));
        assert_eq!(
            projected(&runtime(WorkerSessionKind::ClaudeCard, recorded.clone())).as_deref(),
            Some("2.1.259")
        );
        assert_eq!(
            projected(&runtime(WorkerSessionKind::ClaudeCard, None)),
            None
        );
        // Only a Claude card's runtime has that document; another runtime's state is not read so.
        assert_eq!(
            projected(&runtime(WorkerSessionKind::CodexCard, recorded)),
            None
        );
    }
}
