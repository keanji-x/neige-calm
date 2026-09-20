//! `/internal/codex/hook` — receive codex CLI hook events from the `neige-codex-bridge` subprocess and re-emit them on the WS event bus as `hook.codex.<snake_case_name>`.
//! Mounted under `/internal/*` because the frontend never calls it; the bridge resolves the URL from `NEIGE_CALM_BASE_URL`.

use crate::actor::Actor;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::Terminal;
use crate::role_gate::RoleViolation;
use crate::session_projection_lookup::resolve_session_for_thread;
use crate::session_projection_repo::AgentProvider;
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::post,
};
use calm_types::worker::WorkerSessionId;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn router() -> Router<AppState> {
    Router::new()
        // Loopback-only ingest; the bridge subprocess is spawned by codex itself with env vars pointing here.
        .route("/internal/codex/hook", post(ingest_hook))
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: Option<CardId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HookProvider {
    Codex,
    Claude,
}

impl HookProvider {
    fn kind_prefix(self) -> &'static str {
        match self {
            Self::Codex => "hook.codex",
            Self::Claude => "hook.claude",
        }
    }

    fn actor(self, card_id: CardId) -> ActorId {
        match self {
            Self::Codex => ActorId::AiCodex(card_id),
            Self::Claude => ActorId::AiClaude(card_id),
        }
    }

    fn session_actor(self, session_id: WorkerSessionId) -> ActorId {
        match self {
            Self::Codex => ActorId::AiCodexSession(session_id),
            Self::Claude => ActorId::AiClaudeSession(session_id),
        }
    }

    fn event(
        self,
        card_id: CardId,
        kind: String,
        payload: Value,
        hook_idempotency_key: String,
    ) -> Event {
        match self {
            Self::Codex => Event::CodexHook {
                card_id,
                kind,
                payload,
                hook_idempotency_key,
            },
            Self::Claude => Event::ClaudeHook {
                card_id,
                kind,
                payload,
                hook_idempotency_key,
            },
        }
    }

    fn into_agent_provider(self) -> AgentProvider {
        match self {
            Self::Codex => AgentProvider::Codex,
            Self::Claude => AgentProvider::Claude,
        }
    }
}

/// Loopback-only ingest: extract `hook_event_name`, tag it, and emit on the bus through `Repo::log_pure_event`, so every hook payload is recorded verbatim in the audit/replay store.
/// The middleware's `"user"` fallback is deliberately kept: an older bridge with no header is the only way to hit it, and re-attributing silently would be dishonest.
pub(crate) async fn ingest_hook(
    State(s): State<RouteState>,
    _actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    let card_id = resolve_ingest_card_id(q.card_id)?;
    ingest_provider_hook(&s, card_id, payload, HookProvider::Codex).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn resolve_ingest_card_id(card_id: Option<CardId>) -> Result<String> {
    let Some(card_id) = card_id else {
        return Err(empty_ai_card_id());
    };
    if card_id.as_str().is_empty() {
        return Err(empty_ai_card_id());
    }
    Ok(card_id.0)
}

fn empty_ai_card_id() -> CalmError {
    CalmError::Forbidden(RoleViolation::EmptyAiCardId.to_string())
}

#[allow(deprecated)]
pub(crate) async fn ingest_provider_hook(
    s: &RouteState,
    card_id_str: String,
    payload: Value,
    provider: HookProvider,
) -> Result<()> {
    let card_id_typed = CardId::from(card_id_str.clone());
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("{}.{}", provider.kind_prefix(), to_snake_case(event_name));
    let hook_idempotency_key = hook_idempotency_key(provider, &card_id_str, &payload);

    // A hook for a Planner-opened terminal is advisory telemetry, never worker state: it is appended to the live renderer entry's ring and acknowledged BEFORE the worker dedupe cache and the persist / FSM path, so it can never move a card FSM or evict a worker key.
    // The discriminator is the creation-time `TERMINAL_SIGNALS_PAYLOAD_KEY` in the card payload (survives a `kind` PATCH and the terminal row's deletion), then `kind == "terminal"`; owning a terminal row is NOT it, since Worker cards own one too.
    let card = s.repo.card_get(&card_id_str).await?;
    if card.as_ref().is_some_and(is_planner_terminal_card)
        || card.as_ref().is_some_and(|card| card.kind == "terminal")
    {
        let terminal = s.repo.terminal_get_by_card(&card_id_str).await?;
        return ingest_terminal_signal(
            s,
            &card_id_str,
            terminal,
            &payload,
            provider,
            hook_idempotency_key,
        )
        .await;
    }

    {
        let cache = s
            .hook_ingest_cache
            .lock()
            .expect("hook ingest cache mutex poisoned");
        if cache.contains(&hook_idempotency_key) {
            tracing::warn!(
                target: "hook.ingest.dedupe",
                provider = ?provider,
                key = %hook_idempotency_key,
                "duplicate hook ingest suppressed"
            );
            return Ok(());
        }
    }

    let resolved_session = cross_check_session_card(s, &card_id_str, &payload, provider).await?;

    // Stamp `ActorId::AiCodex(CardId)`; the role gate's empty-CardId guard catches an unresolvable `card_id`. Fall back to `EventScope::System` when the card has been deleted, and the gate then refuses the write — a hook for a deleted card is an audit smell.
    let scope = match card {
        Some(c) => match s.repo.track_get(c.track_id.as_str()).await? {
            Some(w) => EventScope::Card {
                card: c.id,
                track: w.id,
                area: w.area_id,
            },
            None => EventScope::System,
        },
        None => EventScope::System,
    };

    s.repo
        .log_pure_event(
            resolved_session
                .map(|session_id| provider.session_actor(session_id))
                .unwrap_or_else(|| provider.actor(card_id_typed.clone())),
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.area_cache(),
            provider.event(card_id_typed, kind, payload, hook_idempotency_key.clone()),
        )
        .await?;
    // Concurrent duplicates during this log call may pass; dispatcher watermarks and harness LRU dedupe them.
    s.hook_ingest_cache
        .lock()
        .expect("hook ingest cache mutex poisoned")
        .insert(hook_idempotency_key);
    Ok(())
}

/// Whether `card` was opened by the Planner with hook signals: the creation-time `TERMINAL_SIGNALS_PAYLOAD_KEY == true` marker. Read from the card, never from the terminal row or the patchable `kind`.
pub fn is_planner_terminal_card(card: &crate::model::Card) -> bool {
    card.payload
        .get(crate::validation::TERMINAL_SIGNALS_PAYLOAD_KEY)
        .and_then(Value::as_bool)
        == Some(true)
}

/// Terminal-card branch of [`ingest_provider_hook`]: parse, bound and append the signal to the CURRENT renderer entry. Malformed payloads are logged and acknowledged (the hook must never fail Claude); the ring is idempotent on the key. The worker `hook_ingest_cache` is never touched.
async fn ingest_terminal_signal(
    s: &RouteState,
    card_id: &str,
    terminal: Option<Terminal>,
    payload: &Value,
    provider: HookProvider,
    hook_idempotency_key: String,
) -> Result<()> {
    match (
        provider,
        crate::terminal_hooks::parse_terminal_signal(payload),
    ) {
        (HookProvider::Claude, Ok(incoming)) => match terminal {
            Some(term) => {
                let event = incoming.event.clone();
                let seq = s.terminal_renderer.push_signal(
                    &term.id,
                    &hook_idempotency_key,
                    incoming,
                    crate::model::now_ms(),
                );
                tracing::info!(
                    target: "hook.ingest.terminal_signal",
                    card_id = %card_id,
                    terminal_id = %term.id,
                    event = %event,
                    seq = ?seq,
                    "terminal hook signal appended (None: no live renderer entry or duplicate)"
                );
            }
            None => tracing::warn!(
                target: "hook.ingest.terminal_signal_dropped",
                card_id = %card_id,
                "terminal card has no terminal row; hook signal dropped"
            ),
        },
        (HookProvider::Codex, _) => tracing::warn!(
            target: "hook.ingest.terminal_signal_dropped",
            card_id = %card_id,
            "codex hook for a terminal card; accepted and ignored"
        ),
        (_, Err(reason)) => tracing::warn!(
            target: "hook.ingest.terminal_signal_dropped",
            card_id = %card_id,
            reason = %reason,
            "malformed or unknown hook payload for a terminal card; accepted and ignored"
        ),
    }
    Ok(())
}

async fn cross_check_session_card(
    s: &RouteState,
    card_id_str: &str,
    payload: &Value,
    provider: HookProvider,
) -> Result<Option<WorkerSessionId>> {
    let Some(session_id) = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
    else {
        tracing::info!(
            target: "hook.ingest.no_session",
            provider = ?provider,
            query_card = %card_id_str,
            "hook ingest proceeding without payload session_id"
        );
        return Ok(None);
    };

    let Some((worker_session_id, resolved_card)) =
        resolve_session_for_thread(s.repo.as_ref(), provider.into_agent_provider(), session_id)
            .await?
    else {
        return Ok(None);
    };
    if resolved_card != card_id_str {
        tracing::warn!(
            target: "hook.ingest.card_mismatch",
            provider = ?provider,
            query_card = %card_id_str,
            payload_card = %resolved_card,
            session_id = %session_id,
            "hook ingest rejected: session_id maps to different card"
        );
        return Err(CalmError::BadRequest(
            "hook session_id/card_id mismatch".into(),
        ));
    }

    Ok(Some(worker_session_id))
}

fn hook_idempotency_key(provider: HookProvider, card_id: &str, payload: &Value) -> String {
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let hook_event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let body_hash = payload_body_hash(payload);
    let primary = format!(
        "{prov}|{card}|{session_id}|{hook_event}|{body_hash}",
        prov = provider.kind_prefix(),
        card = card_id
    );
    sha256_hex(&primary)
}

fn payload_body_hash(payload: &Value) -> String {
    let bytes = serde_json::to_vec(payload).expect("serde_json::Value serialization is infallible");
    sha256_bytes(&bytes)
}

fn sha256_hex(text: &str) -> String {
    sha256_bytes(text.as_bytes())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// One row of the codex CLI hook table; `docker/codex-requirements.toml` registers exactly these.
/// Event-name vocabulary only: no hook moves a card's state (a card's activity comes from its PTY output).
pub struct CodexWorkerHook {
    /// PascalCase event name, used verbatim as the key in docker/codex-requirements.toml.
    pub event_name: &'static str,
}

pub const CODEX_WORKER_HOOKS: &[CodexWorkerHook] = &[
    CodexWorkerHook {
        event_name: "SessionStart",
    },
    CodexWorkerHook {
        event_name: "UserPromptSubmit",
    },
    CodexWorkerHook {
        event_name: "PreToolUse",
    },
    CodexWorkerHook {
        event_name: "PostToolUse",
    },
    CodexWorkerHook {
        event_name: "PermissionRequest",
    },
    CodexWorkerHook { event_name: "Stop" },
];

/// Convert codex's `PascalCase` event names (`PreToolUse`) to snake, matching the Claude hook discriminators on the wire.
pub(crate) fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX_REQUIREMENTS_TOML: &str =
        include_str!("../../../../docker/codex-requirements.toml");

    #[test]
    fn every_codex_worker_hook_is_registered_in_requirements_toml() {
        for hook in CODEX_WORKER_HOOKS {
            let needle = format!("[[hooks.{}]]", hook.event_name);
            assert!(
                CODEX_REQUIREMENTS_TOML.contains(&needle),
                "docker/codex-requirements.toml is missing registration for {needle}; \
                 the kernel expects this hook but codex CLI never fires it. See #372.",
            );
        }
    }

    #[test]
    fn snake_case_examples() {
        assert_eq!(to_snake_case("PreToolUse"), "pre_tool_use");
        assert_eq!(to_snake_case("Stop"), "stop");
        assert_eq!(to_snake_case("SessionStart"), "session_start");
        assert_eq!(to_snake_case("unknown"), "unknown");
    }

    #[test]
    fn resolve_ingest_card_id_rejects_absent_and_empty() {
        let expected = RoleViolation::EmptyAiCardId.to_string();
        for card_id in [None, Some(CardId::from(""))] {
            match resolve_ingest_card_id(card_id) {
                Err(CalmError::Forbidden(message)) => assert_eq!(message, expected),
                other => panic!("expected forbidden EmptyAiCardId, got {other:?}"),
            }
        }
    }

    #[test]
    fn resolve_ingest_card_id_preserves_raw_inner_string() {
        assert_eq!(
            resolve_ingest_card_id(Some(CardId::from(" card-1 "))).unwrap(),
            " card-1 "
        );
    }

    #[test]
    fn hook_key_uses_session_primary_without_transcript_metadata() {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
        });

        let first = hook_idempotency_key(HookProvider::Codex, "card-1", &payload);
        let second = hook_idempotency_key(HookProvider::Codex, "card-1", &payload);
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn hook_key_primary_includes_event_name() {
        let stop = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
        });
        let pre_tool = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
        });

        let stop_key = hook_idempotency_key(HookProvider::Codex, "card-1", &stop);
        let pre_tool_key = hook_idempotency_key(HookProvider::Codex, "card-1", &pre_tool);
        assert_ne!(stop_key, pre_tool_key);
    }

    #[test]
    fn hook_key_distinguishes_by_body_hash() {
        let first_payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "exit_code": 0,
        });
        let second_payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "exit_code": 1,
        });

        let first = hook_idempotency_key(HookProvider::Codex, "card-1", &first_payload);
        let second = hook_idempotency_key(HookProvider::Codex, "card-1", &second_payload);
        assert_ne!(first, second);
    }

    #[test]
    fn hook_key_fallback_is_stable() {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
        });

        let first = hook_idempotency_key(HookProvider::Codex, "card-1", &payload);
        let second = hook_idempotency_key(HookProvider::Codex, "card-1", &payload);
        assert_eq!(first, second);
    }

    #[test]
    fn hook_key_fallback_includes_event_name() {
        let stop = serde_json::json!({
            "hook_event_name": "Stop",
        });
        let pre_tool = serde_json::json!({
            "hook_event_name": "PreToolUse",
        });

        let stop_key = hook_idempotency_key(HookProvider::Codex, "card-1", &stop);
        let pre_tool_key = hook_idempotency_key(HookProvider::Codex, "card-1", &pre_tool);
        assert_ne!(stop_key, pre_tool_key);
    }

    #[test]
    fn hook_key_fallback_distinguishes_by_body_hash() {
        let first_payload = serde_json::json!({
            "hook_event_name": "Stop",
            "exit_code": 0,
        });
        let second_payload = serde_json::json!({
            "hook_event_name": "Stop",
            "exit_code": 1,
        });

        let first = hook_idempotency_key(HookProvider::Codex, "card-1", &first_payload);
        let second = hook_idempotency_key(HookProvider::Codex, "card-1", &second_payload);
        assert_ne!(first, second);
    }
}
