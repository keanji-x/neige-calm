//! `/api/cards/:id/terminal` — read-side helpers for terminal cards.
//!
//! The companion write path used to live here (`POST /api/cards/:id/terminal`,
//! the second leg of the 3-step terminal-card recipe) but #13's atomic
//! endpoint replaced it. The single remaining route is the GET that
//! `useTodayTerminal` uses to validate a cached `card_id` from
//! `localStorage` before attempting a WS attach.
//!
//! `spawn_terminal_for` stays public because several call sites still need
//! it: terminal/codex/claude card creation and the WS lazy reattach path.

use crate::db::RouteRepo;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Terminal;
use crate::state::{AppState, DaemonClient};
use crate::terminal_renderer::{RendererConfig, RendererEntry, TerminalRendererRegistry};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use std::sync::Arc;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/cards/{card_id}/terminal", get(get_terminal_for_card))
}

/// Look up the Terminal row a card owns. Returns 404 if the card has no
/// terminal (yet). The UI uses this to validate a card_id cached in
/// localStorage before attempting a WS attach to its terminal.
#[utoipa::path(
    get,
    path = "/api/cards/{card_id}/terminal",
    tag = "terminals",
    params(("card_id" = String, Path, description = "Card id (must be a terminal card)")),
    responses(
        (status = 200, description = "Terminal row for this card", body = Terminal),
        (status = 404, description = "Card has no terminal yet", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_terminal_for_card(
    State(s): State<AppState>,
    Path(card_id): Path<String>,
) -> Result<Json<Terminal>> {
    let term = s
        .repo
        .terminal_get_by_card(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal for card {card_id}")))?;
    Ok(Json(term))
}

/// Ensure a renderer-backed terminal process exists for the given terminal
/// row. Used by terminal/codex card creation and lazy WS reattach.
pub(crate) async fn spawn_terminal_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<Arc<RendererEntry>> {
    spawn_terminal_with_parts(
        s.daemon.as_ref(),
        s.terminal_renderer.as_ref(),
        s.repo.as_ref(),
        term,
        program,
        cwd,
        env,
    )
    .await
}

/// PR6 (#136) — lower-level seam over `spawn_terminal_for` that takes the
/// constituent `DaemonClient` + `&dyn RouteRepo` instead of the full
/// `AppState`. Used by the dispatcher (which doesn't own an `AppState` —
/// it's a kernel-internal worker that ships before AppState exists in the
/// boot order).
pub(crate) async fn spawn_terminal_with_parts(
    daemon: &DaemonClient,
    renderer: &TerminalRendererRegistry,
    _repo: &dyn RouteRepo,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<Arc<RendererEntry>> {
    #[cfg(feature = "fixtures")]
    if matches!(
        std::env::var("FAKE_CODEX_PTY_FAIL").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    ) {
        return Err(CalmError::Internal(
            "forced PTY spawn failure via FAKE_CODEX_PTY_FAIL".into(),
        ));
    }

    let proc_supervisor_sock =
        crate::proc_supervisor::resolve_control_sock(daemon.proc_supervisor_sock.as_deref())
            .await?;

    // #177 PR2 — `term.theme_fg/_bg` are the single source of truth for
    // startup OSC 10/11 reply colors. Thread them into every renderer spawn.
    let mut envs = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
    ];
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                envs.push((k.clone(), val.to_string()));
            }
        }
    }

    renderer
        .ensure(RendererConfig {
            terminal_id: term.id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: parse_rgb(&term.theme_fg).map_err(CalmError::Internal)?,
            terminal_bg: parse_rgb(&term.theme_bg).map_err(CalmError::Internal)?,
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), program.to_string()],
            envs,
            cwd: cwd.to_string(),
            supervisor_sock: proc_supervisor_sock,
        })
        .await
        .map_err(|e| CalmError::Internal(e.to_string()))
}

fn parse_rgb(s: &str) -> std::result::Result<(u8, u8, u8), String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected `r,g,b` (three comma-separated u8 channels), got {s:?}"
        ));
    }
    let parse = |i: usize| -> std::result::Result<u8, String> {
        parts[i]
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("channel {i} ({:?}): {e}", parts[i]))
    };
    Ok((parse(0)?, parse(1)?, parse(2)?))
}
