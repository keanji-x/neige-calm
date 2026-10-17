//! `/api/cards/:id/terminal` — read-side helpers for terminal cards, plus the spawn seam card
//! creation uses and the attach-only seam WS lazy reattach uses.

use crate::db::RouteRepo;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path};
use crate::model::Terminal;
use crate::state::{AppState, DaemonClient, RouteState};
use crate::terminal_renderer::{RendererConfig, RendererEntry, TerminalRendererRegistry};
use axum::{Router, extract::State, routing::get};
use std::sync::Arc;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/cards/{card_id}/terminal", get(get_terminal_for_card))
}

/// Look up the Terminal row a card owns; 404 if the card has no terminal (yet).
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
    State(s): State<RouteState>,
    Path(card_id): Path<String>,
) -> Result<Json<Terminal>> {
    let term = s
        .repo
        .terminal_get_by_card(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal for card {card_id}")))?;
    Ok(Json(term))
}

/// A renderer on the child the supervisor runs for `term` now; never a start of `term.program`
/// (#2516). A terminal with no running child is an error.
pub(crate) async fn attach_terminal_for(
    s: &AppState,
    term: &Terminal,
) -> Result<Arc<RendererEntry>> {
    if let Some(entry) = attach_original_registered(
        s.daemon.as_ref(),
        s.terminal_renderer.as_ref(),
        s.repo.as_ref(),
        term,
    )
    .await?
    {
        return Ok(entry);
    }
    s.terminal_renderer
        .attach_running(
            terminal_renderer_config(s.daemon.as_ref(), term, &term.program, &term.cwd, &term.env)
                .await?,
        )
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))
}

/// Start a terminal's program from its constituent parts rather than the full `AppState`; used by
/// the operation spawns and the dispatcher, which doesn't own an `AppState`.
pub(crate) async fn spawn_terminal_with_parts(
    daemon: &DaemonClient,
    renderer: &TerminalRendererRegistry,
    repo: &dyn RouteRepo,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<Arc<RendererEntry>> {
    if let Some(entry) = attach_original_registered(daemon, renderer, repo, term).await? {
        return Ok(entry);
    }
    renderer
        .ensure(terminal_renderer_config(daemon, term, program, cwd, env).await?)
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))
}

async fn attach_original_registered(
    daemon: &DaemonClient,
    renderer: &TerminalRendererRegistry,
    repo: &dyn RouteRepo,
    term: &Terminal,
) -> Result<Option<Arc<RendererEntry>>> {
    use crate::operation::terminal_launch::registered::{self, OriginalLaunch};
    match registered::lookup(repo, &term.id).await? {
        OriginalLaunch::Legacy => Ok(None),
        OriginalLaunch::AwaitingOriginalRequest => Err(CalmError::Conflict(
            "Terminal's original registered request is still pending".into(),
        )),
        OriginalLaunch::Registered(original) => {
            let crate::state::TerminalExecutionTransport::Registered(config) = &daemon.execution
            else {
                return Err(CalmError::Conflict(
                    "original Terminal requires its configured registered executor".into(),
                ));
            };
            let execution =
                registered::restore(config, renderer.owning_repository()?, &original).await?;
            let cfg = registered_renderer_config(
                term,
                &term.cwd,
                config.socket(),
                &original.original.request.specification,
            )?;
            Ok(Some(
                renderer
                    .attach_prepared(cfg, execution)
                    .await
                    .map_err(|error| CalmError::Internal(error.to_string()))?,
            ))
        }
    }
}

pub(crate) async fn spawn_task_terminal_with_parts(
    daemon: &DaemonClient,
    renderer: &TerminalRendererRegistry,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
    launch: crate::operation::task_launch::TaskLaunch,
) -> Result<Arc<RendererEntry>> {
    renderer
        .ensure_for_task(
            terminal_renderer_config(daemon, term, program, cwd, env).await?,
            launch,
        )
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))
}

pub(crate) async fn terminal_renderer_config(
    daemon: &DaemonClient,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<RendererConfig> {
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

    let envs = terminal_child_envs(env);
    // #1784: `neige` in a PTY resolves to the running kernel's CLI. The prepend runs inside the
    // shell, so the PATH the proc supervisor hands the child stays byte-exact behind it.
    let path_prepend = crate::kernel_bin_path::shell_path_prepend()
        .map_err(|error| CalmError::Internal(format!("terminal PATH: {error}")))?;

    Ok(RendererConfig {
        terminal_id: term.id.clone(),
        cols: 80,
        rows: 24,
        buffer_bytes: 1 << 20,
        terminal_fg: parse_rgb(&term.theme_fg).map_err(CalmError::Internal)?,
        terminal_bg: parse_rgb(&term.theme_bg).map_err(CalmError::Internal)?,
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), format!("{path_prepend}\n{program}")],
        envs,
        cwd: cwd.to_string(),
        supervisor_sock: proc_supervisor_sock,
    })
}

/// The environment owner freezes the same explicit ambient allowlist used by
/// the existing supervisor, then applies the Terminal's declared overlays.
/// The executor receives this complete set and adds no current ambient values.
pub(crate) fn terminal_execution_specification(
    program: &str,
    env: &serde_json::Value,
) -> Result<calm_session::control::execution::ExecutionSpecification> {
    let mut environment = std::collections::BTreeMap::new();
    for (key, value) in calm_proc_supervisor::allowed_environment() {
        let key = key
            .into_string()
            .map_err(|_| CalmError::Conflict("non-UTF-8 allowed child environment name".into()))?;
        let value = value
            .into_string()
            .map_err(|_| CalmError::Conflict("non-UTF-8 allowed child environment value".into()))?;
        environment.insert(key, value);
    }
    environment.extend(terminal_child_envs(env));
    let prepend = crate::kernel_bin_path::shell_path_prepend()
        .map_err(|error| CalmError::Internal(format!("terminal PATH: {error}")))?;
    Ok(calm_session::control::execution::ExecutionSpecification {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), format!("{prepend}\n{program}")],
        environment: environment.into_iter().collect(),
        io_mode: calm_session::control::IoMode::Pty { cols: 80, rows: 24 },
        replay_bytes: 1 << 20,
    })
}

pub(crate) fn registered_renderer_config(
    term: &Terminal,
    cwd: &str,
    socket: &std::path::Path,
    specification: &calm_session::control::execution::ExecutionSpecification,
) -> Result<RendererConfig> {
    let calm_session::control::IoMode::Pty { cols, rows } = specification.io_mode else {
        return Err(CalmError::Conflict(
            "Terminal original execution is not a PTY".into(),
        ));
    };
    Ok(RendererConfig {
        terminal_id: term.id.clone(),
        cols,
        rows,
        buffer_bytes: specification.replay_bytes,
        terminal_fg: parse_rgb(&term.theme_fg).map_err(CalmError::Internal)?,
        terminal_bg: parse_rgb(&term.theme_bg).map_err(CalmError::Internal)?,
        program: specification.program.to_string_lossy().into_owned(),
        args: specification.args.clone(),
        envs: specification.environment.clone(),
        cwd: cwd.into(),
        supervisor_sock: socket.into(),
    })
}

/// Overlay `TERM` / `COLORTERM` / `TERM_PROGRAM=neige` so a server launched
/// from VS Code does not inherit `TERM_PROGRAM=vscode`. Caller pairs are
/// appended last and win on conflict.
pub(crate) fn terminal_child_envs(env: &serde_json::Value) -> Vec<(String, String)> {
    let mut envs = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        ("TERM_PROGRAM".to_string(), "neige".to_string()),
    ];
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                envs.push((k.clone(), val.to_string()));
            }
        }
    }
    envs
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

#[cfg(test)]
mod tests {
    use super::terminal_child_envs;
    use serde_json::json;

    #[test]
    fn pins_term_program_neige_unless_caller_overrides() {
        let envs = terminal_child_envs(&json!({}));
        assert!(envs.contains(&("TERM_PROGRAM".into(), "neige".into())));
        let over = terminal_child_envs(&json!({ "TERM_PROGRAM": "vscode" }));
        assert_eq!(over.last(), Some(&("TERM_PROGRAM".into(), "vscode".into())));
    }
}
