//! `POST /api/waves/:wave_id/claude-cards` — manual Claude worker card
//! creation.
//!
//! This mirrors the codex card endpoint's PTY-backed shape but deliberately
//! omits all MCP wiring. The spawned process is a resident interactive
//! `claude` TUI with a generated `--settings <path>` file whose hooks call
//! the existing `neige-codex-bridge` in Claude provider mode.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{Card, new_id};
use crate::operation::claude_adapter::{
    ClaudeCreateOperationPayload, ClaudeCreateRequestInput, NormalizedClaudeCreateRequest,
    normalize_claude_create_request as normalize_claude_create_request_payload,
    prepare_claude_create_request,
};
use crate::operation::claude_restart_adapter::ClaudeRestartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::codex_cards::shell_single_quote;
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::runtime_lookup::project_runtime_into_card_payload;
use crate::state::{AppState, CodexShellState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/waves/{wave_id}/claude-cards",
            post(create_claude_card),
        )
        .route("/api/cards/{id}/claude/restart", post(restart_claude_card))
}

/// Body for `POST /api/waves/:wave_id/claude-cards`.
#[derive(Deserialize, Debug, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewClaudeCardBody {
    /// Sort order within the wave. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
    /// Working directory Claude runs in. Empty string or missing -> `$HOME`
    /// (then `cwd` of server).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional first prompt passed as Claude's positional prompt argument.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional card-head logo background CSS color. Empty string is ignored.
    #[serde(default)]
    pub icon_bg: Option<String>,
    /// Optional card-head logo foreground CSS color. Empty string is ignored.
    #[serde(default)]
    pub icon_fg: Option<String>,
    /// Host browser's current theme RGB. Required so the PTY daemon answers
    /// Claude's terminal color probes with colors matching the surrounding UI.
    pub theme: crate::routes::theme::RequestTheme,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/claude-cards",
    tag = "claude",
    params(("wave_id" = String, Path, description = "Wave id to create the Claude card under")),
    request_body(content = NewClaudeCardBody, description = "Body required (theme is mandatory; cwd/prompt optional)"),
    responses(
        (status = 201, description = "Worker card + linked terminal created atomically; Claude daemon spawned", body = Card),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed (rows are persisted; sweeper reaps within ~60s)", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_claude_card(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(wave_id): Path<String>,
    Json(p): Json<NewClaudeCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    let request = normalize_claude_create_request(wave_id, p)?;
    let idempotency_key = parse_idempotency_key_header(&headers)?;
    let prepared =
        prepare_claude_create_request(s.repo.as_ref(), cs.codex.as_ref(), request.clone()).await?;
    let operation_key = new_id();
    let mut hash_env = prepared.env.clone();
    if let Some(map) = hash_env.as_object_mut() {
        map.remove("NEIGE_CARD_ID");
    }
    let runtime_id = new_id();
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
        "env": hash_env,
    }))?;
    let actor = actor.to_actor_id();
    let payload = serde_json::to_value(ClaudeCreateOperationPayload {
        actor,
        runtime_id: Some(runtime_id),
        request: prepared,
    })?;
    let op_id = s
        .operation_runtime
        .submit(
            "claude-create",
            OperationKey {
                operation_key,
                idempotency_key,
                payload_hash,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => {
            let mut card: Card = serde_json::from_value(result)?;
            project_runtime_into_card_payload(s.repo.as_ref(), &mut card).await?;
            Ok((StatusCode::CREATED, Json(card)))
        }
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(calm_error_from_operation_failure(
            last_error_class.as_deref(),
            last_error,
            from_phase,
        )),
        OperationOutcome::Stuck { .. } => {
            Err(CalmError::Internal("operation stuck, see DB".to_string()))
        }
    }
}

pub(crate) fn normalize_claude_create_request(
    wave_id: String,
    body: NewClaudeCardBody,
) -> Result<NormalizedClaudeCreateRequest> {
    normalize_claude_create_request_payload(ClaudeCreateRequestInput {
        wave_id,
        sort: body.sort,
        cwd: body.cwd,
        prompt: body.prompt,
        icon_bg: body.icon_bg,
        icon_fg: body.icon_fg,
        theme: body.theme,
    })
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/claude/restart",
    tag = "claude",
    params(("id" = String, Path, description = "Claude card id")),
    responses(
        (status = 200, description = "Claude card restarted through the existing session", body = Card),
        (status = 403, description = "Card is not a Claude card or lacks resumable Claude metadata", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "Claude child is still active; kill or wait for child exit before restart", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed; rows persist and sweeper handles cleanup", body = ErrorBody),
    ),
)]
pub(crate) async fn restart_claude_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<Card>> {
    let operation_key = new_id();
    let runtime_id = new_id();
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "card_id": &id,
    }))?;
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor: actor.to_actor_id(),
        runtime_id: Some(runtime_id),
        card_id: id,
    })?;
    let op_id = s
        .operation_runtime
        .submit(
            "claude-restart",
            OperationKey {
                operation_key,
                idempotency_key: None,
                payload_hash,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => {
            let mut card: Card = serde_json::from_value(result)?;
            project_runtime_into_card_payload(s.repo.as_ref(), &mut card).await?;
            Ok(Json(card))
        }
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(calm_error_from_operation_failure(
            last_error_class.as_deref(),
            last_error,
            from_phase,
        )),
        OperationOutcome::Stuck { .. } => {
            Err(CalmError::Internal("operation stuck, see DB".to_string()))
        }
    }
}

pub(crate) fn claude_hook_command(bridge_bin: &str, card_id: &str, base_url: &str) -> String {
    let hook_url = format!(
        "{}/internal/claude/hook?card_id={}",
        base_url.trim_end_matches('/'),
        card_id
    );
    format!(
        "NEIGE_HOOK_PROVIDER=claude NEIGE_CARD_ID={} NEIGE_CALM_BASE_URL={} NEIGE_HOOK_URL={} {} --provider claude",
        shell_single_quote(card_id),
        shell_single_quote(base_url),
        shell_single_quote(&hook_url),
        shell_single_quote(bridge_bin),
    )
}

pub(crate) fn build_claude_settings_json(hook_command: &str) -> String {
    let hook = json!({ "type": "command", "command": hook_command });
    let mut hooks = serde_json::Map::new();
    for h in crate::card_fsm::CLAUDE_WORKER_HOOKS {
        let group = if h.matcher {
            json!({ "matcher": "*", "hooks": [hook.clone()] })
        } else {
            json!({ "hooks": [hook.clone()] })
        };
        hooks.insert(h.event_name.to_string(), json!([group]));
    }
    let value = json!({ "hooks": serde_json::Value::Object(hooks) });
    serde_json::to_string_pretty(&value).expect("claude settings serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_uses_claude_hook_schema_and_matchers() {
        let s = build_claude_settings_json("bridge --provider claude");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["matcher"],
            serde_json::Value::String("*".into())
        );
        assert_eq!(
            v["hooks"]["PostToolUseFailure"][0]["matcher"],
            serde_json::Value::String("*".into())
        );
        assert!(v["hooks"]["Stop"][0].get("matcher").is_none());
        assert!(v["hooks"]["SessionEnd"][0].get("matcher").is_none());
        assert_eq!(
            v["hooks"]["Notification"][0]["hooks"][0]["command"],
            "bridge --provider claude"
        );
    }

    #[test]
    fn settings_registers_exactly_the_fsm_projected_hooks() {
        use std::collections::BTreeSet;

        let s = build_claude_settings_json("bridge --provider claude");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let registered: BTreeSet<String> = v["hooks"]
            .as_object()
            .expect("hooks is an object")
            .keys()
            .cloned()
            .collect();
        let expected: BTreeSet<String> = crate::card_fsm::CLAUDE_WORKER_HOOKS
            .iter()
            .map(|h| h.event_name.to_string())
            .collect();
        // Settings must register every hook the FSM projects (so Claude actually
        // fires it) and nothing it ignores. #364: this set drifted before.
        assert_eq!(registered, expected);
        // Matcher presence per hook must match the table flag.
        for h in crate::card_fsm::CLAUDE_WORKER_HOOKS {
            let has_matcher = v["hooks"][h.event_name][0].get("matcher").is_some();
            assert_eq!(
                has_matcher, h.matcher,
                "matcher mismatch for {}: settings has_matcher={has_matcher}, table={}",
                h.event_name, h.matcher
            );
        }
    }

    #[test]
    fn hook_command_carries_provider_card_and_base_url() {
        let command = claude_hook_command("/bin/neige-codex-bridge", "card-1", "http://x");
        assert!(command.contains("NEIGE_HOOK_PROVIDER=claude"));
        assert!(command.contains("NEIGE_CARD_ID='card-1'"));
        assert!(command.contains("NEIGE_CALM_BASE_URL='http://x'"));
        assert!(command.contains("NEIGE_HOOK_URL='http://x/internal/claude/hook?card_id=card-1'"));
        assert!(command.contains("'/bin/neige-codex-bridge' --provider claude"));
    }
}
