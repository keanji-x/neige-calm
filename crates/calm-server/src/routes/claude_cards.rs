//! `POST /api/waves/:wave_id/claude-cards` — manual Claude worker card
//! creation.
//!
//! This mirrors the codex card endpoint's PTY-backed shape but deliberately
//! omits all MCP wiring. The spawned process is a resident interactive
//! `claude` TUI with a generated `--settings <path>` file whose hooks call
//! the existing `neige-codex-bridge` in Claude provider mode.

use crate::actor::Actor;
use crate::db::sqlite::card_with_claude_create_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{Card, CardRole, new_id};
use crate::routes::cards::card_scope;
use crate::routes::codex_cards::{default_cwd, normalize_optional_css_color, shell_single_quote};
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_terminal_for;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/waves/{wave_id}/claude-cards",
        post(create_claude_card),
    )
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
pub(crate) async fn create_claude_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(wave_id): Path<String>,
    Json(p): Json<NewClaudeCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    if s.repo.wave_get(&wave_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }

    if let Some(raw) = p.cwd.as_deref()
        && raw.chars().any(|c| c.is_ascii_control())
    {
        return Err(CalmError::BadRequest(
            "cwd must not contain ASCII control characters".into(),
        ));
    }
    let icon_bg = normalize_optional_css_color(p.icon_bg.as_deref(), "icon_bg")?;
    let icon_fg = normalize_optional_css_color(p.icon_fg.as_deref(), "icon_fg")?;

    let card_id = new_id();
    let claude_session_id = uuid::Uuid::new_v4().to_string();
    let cwd = p
        .cwd
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(default_cwd);
    let prompt = p
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let settings = load_settings(s.repo.as_ref()).await?;
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        serde_json::Value::String(card_id.clone()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        serde_json::Value::String(s.codex.ingest_url.clone()),
    );
    env_map.insert(
        "NEIGE_HOOK_PROVIDER".to_string(),
        serde_json::Value::String("claude".into()),
    );
    if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
        env_map.insert(
            "HTTP_PROXY".to_string(),
            serde_json::Value::String(p.to_string()),
        );
        env_map.insert(
            "http_proxy".to_string(),
            serde_json::Value::String(p.to_string()),
        );
    }
    if let Some(p) = settings.https_proxy.as_deref().filter(|s| !s.is_empty()) {
        env_map.insert(
            "HTTPS_PROXY".to_string(),
            serde_json::Value::String(p.to_string()),
        );
        env_map.insert(
            "https_proxy".to_string(),
            serde_json::Value::String(p.to_string()),
        );
    }
    let env = serde_json::Value::Object(env_map);

    let settings_dir = s.codex.claude_settings_dir.join(&card_id);
    let settings_path = settings_dir.join("settings.json");
    let settings_path_string = settings_path.to_string_lossy().to_string();
    let mut command_line = format!(
        "{} --settings {} --session-id {}",
        shell_single_quote(&s.codex.claude_bin),
        shell_single_quote(&settings_path_string),
        shell_single_quote(&claude_session_id),
    );
    if let Some(p) = prompt.as_deref() {
        command_line.push_str(" -- ");
        command_line.push_str(&shell_single_quote(p));
    }

    let sort = p.sort;
    let card_id_for_tx = card_id.clone();
    let command_line_for_tx = command_line.clone();
    let cwd_for_tx = cwd.clone();
    let env_for_tx = env.clone();
    let prompt_for_tx = prompt.clone();
    let icon_bg_for_tx = icon_bg.clone();
    let icon_fg_for_tx = icon_fg.clone();
    let settings_path_for_tx = settings_path_string.clone();
    let claude_session_id_for_tx = claude_session_id.clone();
    let theme_for_tx = p.theme;
    let scope = card_scope(
        s.repo.as_ref(),
        card_id.clone().into(),
        wave_id.clone().into(),
    )
    .await?;
    let wave_id_for_tx = wave_id;
    let cache_for_tx = s.card_role_cache.clone();
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                let (card, _term) = card_with_claude_create_tx(
                    tx,
                    card_id_for_tx,
                    wave_id_for_tx.into(),
                    sort,
                    command_line_for_tx,
                    cwd_for_tx,
                    env_for_tx,
                    prompt_for_tx,
                    icon_bg_for_tx,
                    icon_fg_for_tx,
                    settings_path_for_tx,
                    claude_session_id_for_tx,
                    CardRole::Worker,
                    true,
                    &cache_for_tx,
                    theme_for_tx,
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await?;

    std::fs::create_dir_all(&settings_dir).map_err(|e| {
        CalmError::Internal(format!(
            "mkdir claude settings dir {}: {e}",
            settings_dir.display()
        ))
    })?;
    let hook_command = claude_hook_command(
        &s.codex.bridge_bin.to_string_lossy(),
        &card_id,
        &s.codex.ingest_url,
    );
    let settings_json = build_claude_settings_json(&hook_command);
    std::fs::write(&settings_path, settings_json)
        .map_err(|e| CalmError::Internal(format!("write claude settings.json: {e}")))?;

    let term = s
        .repo
        .terminal_get_by_card(card.id.as_ref())
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "terminal vanished after commit for card {}",
                card.id
            ))
        })?;

    spawn_terminal_for(&s, &term, &command_line, &cwd, &env).await?;

    tracing::info!(
        card_id = %card.id,
        terminal_id = %term.id,
        cwd = %cwd,
        settings = %settings_path_string,
        claude_session_id = %claude_session_id,
        has_prompt = prompt.is_some(),
        "spawned interactive claude worker"
    );

    Ok((StatusCode::CREATED, Json(card)))
}

fn claude_hook_command(bridge_bin: &str, card_id: &str, base_url: &str) -> String {
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
