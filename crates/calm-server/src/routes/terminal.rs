//! `/api/cards/:id/terminal` — create the Terminal row for a "terminal" card
//! and spawn its `calm-session-daemon`. **Owned by Track D.**
//!
//! ## Flow
//!
//! 1. Validate the card exists and its `kind == "terminal"`.
//! 2. Resolve defaults: empty `program` → `$SHELL` (fallback `/bin/sh`);
//!    empty `cwd` → `$HOME` (fallback server cwd).
//! 3. Persist the row via `repo.terminal_create` so we own a stable
//!    terminal id (used as the socket filename).
//! 4. Spawn `calm-session-daemon` with `--id <uuid> --sock <path>
//!    --cwd <cwd> -- /bin/sh -c <program>`. The daemon binds the socket
//!    and writes a "ready" marker to a pipe fd we hand it; here we just
//!    poll the socket path for connectability.
//! 5. Stamp `daemon_handle` on the Terminal row to the socket path so the
//!    WS half can find it on `/api/terminals/:id` without recomputing.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{NewTerminal, Terminal};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/cards/{card_id}/terminal",
        post(create_terminal).get(get_terminal_for_card),
    )
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

#[derive(Deserialize, Debug, Default, ToSchema)]
pub struct NewTerminalBody {
    /// Empty string or missing → `$SHELL` (then `/bin/sh`).
    #[serde(default)]
    pub program: String,
    /// Empty string or missing → `$HOME` (then cwd of server).
    #[serde(default)]
    pub cwd: String,
    /// Extra env on top of the inherited set. JSON object: `{"FOO":"bar"}`.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
}

#[utoipa::path(
    post,
    path = "/api/cards/{card_id}/terminal",
    tag = "terminals",
    params(("card_id" = String, Path, description = "Card id (must be a terminal card)")),
    request_body(content = NewTerminalBody, description = "Optional body — empty means use defaults"),
    responses(
        (status = 201, description = "Terminal created and daemon spawned", body = Terminal),
        (status = 400, description = "Card is not a terminal card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed", body = ErrorBody),
    ),
)]
pub(crate) async fn create_terminal(
    State(s): State<AppState>,
    Path(card_id): Path<String>,
    body: Option<Json<NewTerminalBody>>,
) -> Result<(StatusCode, Json<Terminal>)> {
    let Json(p) = body.unwrap_or_default();

    // 1. Card exists and is a terminal.
    let card = s
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if card.kind != "terminal" {
        return Err(CalmError::BadRequest(format!(
            "card {card_id} kind={} (need 'terminal')",
            card.kind
        )));
    }

    // 2. Defaults.
    let program = if p.program.trim().is_empty() {
        default_program()
    } else {
        p.program
    };
    let cwd = if p.cwd.trim().is_empty() {
        default_cwd()
    } else {
        p.cwd
    };
    let env = if p.env.is_null() {
        serde_json::json!({})
    } else {
        p.env
    };

    // 3. Persist the row.
    let term = s
        .repo
        .terminal_create(NewTerminal {
            card_id: card_id.clone(),
            program: program.clone(),
            cwd: cwd.clone(),
            env: env.clone(),
        })
        .await?;

    // 4. Spawn the daemon and stamp the socket path back onto the row.
    spawn_daemon_for(&s, &term, &program, &cwd, &env).await?;
    let term = s
        .repo
        .terminal_get(&term.id)
        .await?
        .ok_or_else(|| CalmError::Internal("terminal vanished after create".into()))?;

    Ok((StatusCode::CREATED, Json(term)))
}

/// Spawn a `calm-session-daemon` for the given terminal row, wait for its
/// unix socket to accept connections, and persist the socket path as the
/// row's `daemon_handle`. Used by `create` and (when a previously-spawned
/// daemon has died) by the WS handler's auto-revive path.
pub(crate) async fn spawn_daemon_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    let sock = s.daemon.sock_path(&term.id);
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CalmError::Internal(format!("mkdir sock parent: {e}")))?;
    }
    // Stale leftover socket file from a previous daemon — must remove or
    // bind() refuses.
    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }
    let sock_str = sock.to_string_lossy().to_string();

    let mut cmd = tokio::process::Command::new(&s.daemon.session_daemon_bin);
    cmd.args(["--id", &term.id])
        .args(["--sock", &sock_str])
        .args(["--cwd", cwd])
        .arg("--")
        .args(["/bin/sh", "-c", program]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);

    let mut child = cmd
        .spawn()
        .map_err(|e| CalmError::Internal(format!("spawn calm-session-daemon: {e}")))?;
    let pid = child.id();
    tracing::info!(pid = ?pid, terminal_id = %term.id, "spawned calm-session-daemon");
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    // Poll until the daemon accepts connections (or give up after ~3s).
    let mut ready = false;
    for _ in 0..75 {
        if UnixStream::connect(&sock).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    if !ready {
        return Err(CalmError::Internal(format!(
            "daemon for terminal {} did not become ready",
            term.id
        )));
    }
    s.repo
        .terminal_set_handle(&term.id, Some(&sock_str))
        .await?;
    Ok(())
}

fn default_program() -> String {
    let s = std::env::var("SHELL").unwrap_or_default();
    if s.is_empty() {
        "/bin/sh".to_string()
    } else {
        s
    }
}

fn default_cwd() -> String {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return home;
    }
    std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}
