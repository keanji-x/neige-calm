//! `POST /api/waves/:wave_id/codex-cards` — atomic codex-card creation.
//!
//! Structural twin of `routes/terminal_cards.rs` for the codex flow (#117).
//! Collapses what used to be a 2-step recipe — `POST .../cards` (kind=codex,
//! empty payload) followed by `POST /api/cards/:id/codex` (spawn PTY +
//! stamp `terminal_id`) — into a single endpoint:
//!
//! 1. Inside one DB transaction, `card_with_codex_create_tx` writes the
//!    `codex`-kind card, linked `terminal` row, and initial `Starting`
//!    runtime row, stamping `{schemaVersion, terminal_id, cwd?}` onto the
//!    card payload. The transaction also persists the `card.added` event
//!    with the final payload, so a single broadcast carries the fully-formed
//!    card to peers — no `card.updated` follow-up, no intermediate
//!    `payload=null` flash for the renderer's "Codex is starting…"
//!    placeholder to react to.
//! 2. After commit, the handler starts the terminal renderer via the same
//!    `spawn_terminal_for` helper the terminal-card endpoint uses. All codex
//!    cards route through the shared app-server: prompt cards start a shared
//!    thread and attach `codex resume`; empty cards register pending FIFO
//!    attribution and spawn `codex --remote`.
//!    A renderer-start failure returns 500 to the client but does NOT roll
//!    back the persisted rows: the orphan-terminal sweeper reaps them within
//!    ~60s.
//!

use crate::actor::Actor;
use crate::codex_appserver::Notification;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{Card, new_id};
use crate::operation::codex_adapter::{
    CodexCreateOperationPayload, CodexCreateRequestInput, normalize_codex_create_request,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use std::time::Duration;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/waves/{wave_id}/codex-cards", post(create_codex_card))
}

/// Body for `POST /api/waves/:wave_id/codex-cards`.
///
/// Deliberately omits `kind` (always `"codex"`) and `payload` (the kernel
/// stamps `{schemaVersion, terminal_id, cwd?, prompt?}` itself). Empty
/// `cwd` falls back to `$HOME` then the server's cwd.
///
/// `prompt` is the hands-free entry point: when non-empty, the kernel starts
/// a shared thread,
/// persists its id on both the payload and `card_codex_threads`, sends the
/// prompt via `turn/start`, waits for `turn/started` or `turn/completed`,
/// and starts the TUI as `codex resume <thread_id> --remote unix://...`.
///
/// Empty / absent `prompt` reverts to the user-initiated flow: codex
/// boots through the shared remote, the composer is empty, the user types
/// and hits Enter.
///
/// Note: the old `initial_prompt` field (which had been a documented
/// no-op since the codex-TUI port) was removed; serde rejects unknown
/// fields with the default config, so a stale caller that still sends
/// it will get a 422 — that's the intended fail-loud signal to update
/// the caller. The interactive `prompt` channel is the one place
/// callers should be putting text now.
///
/// `theme` is required end-to-end (#177): callers MUST send the host
/// browser's current foreground/background RGB. The renderer uses it so
/// codex's OSC 10/11 startup probe gets matching colors. Forcing it at
/// the type layer means a
/// caller that forgets — the exact bug that motivated this refactor —
/// fails at compile time (TS) or at the deserialize step (Rust/JSON,
/// 422). No `Option`, no `#[serde(default)]`, no implicit fallback.
#[derive(Deserialize, Debug, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCodexCardBody {
    /// Sort order within the wave. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
    /// Working directory codex runs in. Empty string or missing → `$HOME`
    /// (then `cwd` of server).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Hands-free seed prompt. When set and non-empty, codex boots with
    /// its composer pre-filled and the kernel auto-submits the composer
    /// once codex's session is constructed. See the struct doc for the
    /// full mechanism.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional card-head logo background CSS color. Empty string is ignored.
    #[serde(default)]
    pub icon_bg: Option<String>,
    /// Optional card-head logo foreground CSS color. Empty string is ignored.
    #[serde(default)]
    pub icon_fg: Option<String>,
    /// Host browser's current theme RGB (#177). Required so the terminal
    /// model answers codex's OSC 10/11 startup probe with colors matching
    /// the host theme. A caller that omits this field gets 422.
    pub theme: crate::routes::theme::RequestTheme,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/codex-cards",
    tag = "codex",
    params(("wave_id" = String, Path, description = "Wave id to create the codex card under")),
    request_body(content = NewCodexCardBody, description = "Body required (theme is mandatory; cwd/prompt optional)"),
    responses(
        (status = 201, description = "Card + linked terminal created atomically; codex daemon spawned", body = Card),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed (rows are persisted; sweeper reaps within ~60s)", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_codex_card(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Path(wave_id): Path<String>,
    Json(p): Json<NewCodexCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    let request = normalize_codex_create_request(CodexCreateRequestInput {
        wave_id,
        sort: p.sort,
        cwd: p.cwd,
        prompt: p.prompt,
        icon_bg: p.icon_bg,
        icon_fg: p.icon_fg,
        theme: p.theme,
    })?;
    let idempotency_key = parse_idempotency_key_header(&headers)?;
    let operation_key = new_id();
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
    }))?;
    let actor = actor.to_actor_id();
    let payload = serde_json::to_value(CodexCreateOperationPayload { actor, request })?;
    let op_id = s
        .operation_runtime
        .submit(
            "codex-create",
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
            let card: Card = serde_json::from_value(result)?;
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

// ---------------------------------------------------------------------------
// Helpers — moved here from `routes/codex.rs` along with the endpoint they
// support. The remaining `routes/codex.rs` file keeps only the hook-ingest
// loopback route + its query-param struct.
// ---------------------------------------------------------------------------

/// Resolve the codex cwd default. `$HOME` if set, else the server's cwd.
pub(crate) fn default_cwd() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
}

pub(crate) async fn await_shared_initial_turn_lifecycle(
    rx: &mut tokio::sync::broadcast::Receiver<Notification>,
    thread_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(CalmError::CodexAppServer(format!(
                "timed out awaiting initial turn lifecycle notification for shared thread {thread_id}"
            )));
        }
        match tokio::time::timeout(deadline - now, rx.recv()).await {
            Ok(Ok(
                Notification::TurnStarted { thread_id: t, .. }
                | Notification::TurnCompleted { thread_id: t, .. },
            )) if t == thread_id => return Ok(()),
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!(
                    skipped = n,
                    thread_id,
                    "shared prompt card lifecycle subscriber lagged"
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err(CalmError::CodexAppServer(format!(
                    "shared app-server notification channel closed before initial lifecycle for {thread_id}"
                )));
            }
            Err(_) => {
                return Err(CalmError::CodexAppServer(format!(
                    "timed out awaiting initial turn lifecycle notification for shared thread {thread_id}"
                )));
            }
        }
    }
}

/// Wrap a string in POSIX-shell single quotes, escaping any embedded
/// single quotes by closing the quote, emitting a backslash-quoted
/// literal `'\''`, then reopening. Used to pass an arbitrary user
/// prompt to codex as a positional arg without `sh -c` re-interpreting
/// metacharacters. The output is a single shell word.
///
/// Examples:
///   - `hello` → `'hello'`
///   - `she said 'hi'` → `'she said '\''hi'\'''`
///   - `$(rm -rf /)` → `'$(rm -rf /)'` (literal, not expanded by sh)
pub(crate) fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub(crate) fn normalize_optional_css_color(
    raw: Option<&str>,
    field: &str,
) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 128 {
        return Err(CalmError::BadRequest(format!(
            "{field} must be at most 128 bytes"
        )));
    }
    if trimmed.chars().any(|c| c.is_ascii_control()) {
        return Err(CalmError::BadRequest(format!(
            "{field} must not contain ASCII control characters"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_single_quote_basic() {
        assert_eq!(shell_single_quote("hello"), "'hello'");
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn shell_single_quote_embedded_single_quote() {
        // `she said 'hi'` → close, escape, reopen — single shell word.
        assert_eq!(
            shell_single_quote("she said 'hi'"),
            "'she said '\\''hi'\\'''"
        );
    }

    #[test]
    fn shell_single_quote_metacharacters_are_literal() {
        // Defends against `sh -c "codex $promptArg"` re-interpreting
        // `$(...)`, backticks, `;`, `&&`, `|`, etc. The whole arg is
        // inside single quotes so sh ships it as one literal word.
        let prompt = "$(rm -rf /) `whoami` ; echo pwned && true | cat";
        let quoted = shell_single_quote(prompt);
        assert!(quoted.starts_with('\''));
        assert!(quoted.ends_with('\''));
        // Single quotes never appear unescaped inside the body —
        // if they did, sh would close our quoting and the leftover
        // bytes would be re-parsed.
        let body = &quoted[1..quoted.len() - 1];
        for window in body.as_bytes().windows(1) {
            if window == b"'" {
                panic!("unescaped single quote inside body: {body}");
            }
        }
    }
}
