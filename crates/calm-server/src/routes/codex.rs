//! `/api/cards/:id/codex` — create a Codex agent for a `kind == "codex"` card.
//!
//! The kernel doesn't persist a codex row (codex has no PTY socket / no
//! reattach contract — restarts re-spawn from scratch). Instead, hook
//! events are streamed via the WS event bus into the codex card so the
//! UI can render them.
//!
//! ## Flow
//!
//! 1. Validate the card exists and is `kind == "codex"`.
//! 2. `mktemp -d` a per-spawn `CODEX_HOME`. **Seed it** by copying the
//!    contents of `~/.codex` so auth.json / config.toml carry over;
//!    overwrite `hooks.json` to point every event at our bridge.
//! 3. `spawn codex exec` with the temp `CODEX_HOME`, plus envs the bridge
//!    needs to POST back: `NEIGE_CARD_ID`, `NEIGE_CALM_BASE_URL`.
//! 4. Detach. The codex process owns its own lifetime; the WS bus
//!    delivers hooks until the agent exits.
//!
//! Cleanup: `tempfile::TempDir` would auto-clean on drop, but the spawned
//! codex process out-lives the request handler, so we move ownership of
//! the tempdir into the wait-task and let it drop when codex exits.
//!
//! ## Hook bridge
//!
//! Internal ingest is at `POST /internal/codex/hook?card_id=<id>`. The
//! bridge binary (`neige-codex-bridge`) is spawned by codex with stdin =
//! a single JSON object per event; we tag it with the card id (from the
//! query string, not the payload — codex doesn't know about cards) and
//! emit `Event::CodexHook` to the bus.

use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::Card;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path as StdPath;
use std::process::Stdio;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cards/{card_id}/codex", post(create_codex))
        // Loopback-only ingest. The bridge subprocess is spawned by codex
        // itself with env vars pointing here. Not exposed under `/api/*`
        // because the frontend never calls it directly.
        .route("/internal/codex/hook", post(ingest_hook))
}

#[derive(Deserialize, Debug, Default, ToSchema)]
pub struct NewCodexBody {
    /// Required — the first user prompt. Codex's `exec` flow accepts it
    /// as a positional arg.
    pub initial_prompt: String,
    /// Optional override (default codex picks per its own config).
    #[serde(default)]
    pub model: Option<String>,
    /// Working directory codex runs in. Defaults to `$HOME` if empty.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Reserved for a future codex permission/sandbox flag — not wired
    /// yet (the right `-c` config keys for `approval_policy` / sandbox
    /// are version-sensitive). We accept the field so the schema-form on
    /// the frontend can collect it without errors.
    #[serde(default)]
    pub permission_mode: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/cards/{card_id}/codex",
    tag = "codex",
    params(("card_id" = String, Path, description = "Card id (must be a codex card)")),
    request_body(content = NewCodexBody, description = "Codex spawn parameters"),
    responses(
        (status = 202, description = "Codex spawned; hook events stream over WS", body = Card),
        (status = 400, description = "Card is not a codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Spawn failed", body = ErrorBody),
    ),
)]
pub(crate) async fn create_codex(
    State(s): State<AppState>,
    Path(card_id): Path<String>,
    body: Option<Json<NewCodexBody>>,
) -> Result<(StatusCode, Json<Card>)> {
    let Json(p) = body.unwrap_or_default();

    let card = s
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if card.kind != "codex" {
        return Err(CalmError::BadRequest(format!(
            "card {card_id} kind={} (need 'codex')",
            card.kind
        )));
    }
    if p.initial_prompt.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "initial_prompt is required".to_string(),
        ));
    }

    spawn_codex_for(&s, &card_id, &p).await?;

    // The card payload stays minimal — codex has no row to point at; the
    // UI subscribes to `card:<id>` for hook events. We just echo the card
    // back so the client confirms binding succeeded.
    Ok((StatusCode::ACCEPTED, Json(card)))
}

/// Build a fresh `CODEX_HOME` tempdir for this spawn, seed it from
/// `$HOME/.codex` (so user auth + config carry through), overwrite
/// `hooks.json` to point at our bridge, and `codex exec` against it.
async fn spawn_codex_for(s: &AppState, card_id: &str, p: &NewCodexBody) -> Result<()> {
    // 1. Per-spawn CODEX_HOME — never touch the user's real ~/.codex.
    let codex_home = tempfile::Builder::new()
        .prefix("neige-codex-")
        .tempdir()
        .map_err(|e| CalmError::Internal(format!("mktemp codex_home: {e}")))?;

    // 2. Seed from $HOME/.codex if present. Best-effort: a fresh user
    //    without any codex config still works — codex will create
    //    whatever files it needs in our tempdir.
    if let Some(src) = host_codex_dir()
        && src.exists()
    {
        if let Err(e) = copy_dir_recursive(&src, codex_home.path()) {
            tracing::warn!(error = %e, src = %src.display(), "codex seed copy failed; continuing without it");
        }
    }

    // 3. Overwrite hooks.json — even if the seed brought one in, ours
    //    has to win so codex calls our bridge.
    let bridge_path = s.codex.bridge_bin.to_string_lossy().to_string();
    let hooks_json = build_hooks_json(&bridge_path);
    let hooks_path = codex_home.path().join("hooks.json");
    std::fs::write(&hooks_path, hooks_json)
        .map_err(|e| CalmError::Internal(format!("write hooks.json: {e}")))?;

    // 4. Spawn codex.
    let cwd = p
        .cwd
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(default_cwd);

    let mut cmd = tokio::process::Command::new(&s.codex.codex_bin);
    cmd.arg("exec");
    if let Some(model) = p.model.as_deref().filter(|s| !s.trim().is_empty()) {
        cmd.args(["--model", model]);
    }
    cmd.arg("--").arg(&p.initial_prompt);
    cmd.current_dir(&cwd);
    cmd.env("CODEX_HOME", codex_home.path());
    cmd.env("NEIGE_CARD_ID", card_id);
    cmd.env("NEIGE_CALM_BASE_URL", &s.codex.ingest_url);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);

    let mut child = cmd
        .spawn()
        .map_err(|e| CalmError::Internal(format!("spawn codex: {e}")))?;
    let pid = child.id();
    tracing::info!(pid = ?pid, card_id = %card_id, "spawned codex");

    // Move tempdir ownership into the wait task so it stays alive while
    // codex is running; auto-cleans on process exit.
    tokio::spawn(async move {
        let _guard = codex_home; // dropped after `wait` returns
        let _ = child.wait().await;
    });

    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: String,
}

/// Loopback-only ingest. The bridge subprocess POSTs the raw codex hook
/// payload here; we extract `hook_event_name`, tag it, and emit on the
/// bus. No persistence — codex card UIs are append-only ephemeral.
pub(crate) async fn ingest_hook(
    State(s): State<AppState>,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("hook.codex.{}", to_snake_case(event_name));

    s.events.emit(Event::CodexHook {
        card_id: q.card_id,
        kind,
        payload,
    });
    Ok(StatusCode::NO_CONTENT)
}

/// `~/.codex` on the host — visible inside the docker container thanks to
/// the `${HOME}:${HOME}` bind mount in docker-compose.yml.
fn host_codex_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex"))
}

fn default_cwd() -> String {
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

/// Recursively copy `src` to `dst`. Minimal walker — no symlink chasing,
/// no perm propagation beyond what `std::fs::copy` does. Enough to seed
/// `auth.json` / `config.toml` / `models_cache.json` and any sibling
/// dirs codex caches there.
fn copy_dir_recursive(src: &StdPath, dst: &StdPath) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // Symlinks intentionally skipped — copying them would either chase
        // them (potential cycles into $HOME) or leave dangling references
        // inside the tempdir. Codex's own files aren't symlinks.
    }
    Ok(())
}

fn build_hooks_json(bridge: &str) -> String {
    // Bridge command — the binary path resolved by `state::CodexClient`.
    // Codex spec: each hook entry is `{"type":"command", "command":"<argv>"}`.
    // We rely on PATH lookup if `bridge` is a bare name.
    let cmd = serde_json::to_string(bridge).unwrap_or_else(|_| String::from("\"neige-codex-bridge\""));
    format!(
        r#"{{
  "hooks": {{
    "SessionStart":     [{{ "hooks": [{{ "type": "command", "command": {c} }}] }}],
    "PreToolUse":       [{{ "matcher": ".*", "hooks": [{{ "type": "command", "command": {c} }}] }}],
    "PostToolUse":      [{{ "matcher": ".*", "hooks": [{{ "type": "command", "command": {c} }}] }}],
    "PermissionRequest":[{{ "hooks": [{{ "type": "command", "command": {c} }}] }}],
    "UserPromptSubmit": [{{ "hooks": [{{ "type": "command", "command": {c} }}] }}],
    "Stop":             [{{ "hooks": [{{ "type": "command", "command": {c} }}] }}]
  }}
}}
"#,
        c = cmd
    )
}

/// Convert codex's `PascalCase` event names (`PreToolUse`) to snake.
/// Keeps the same shape as Claude hook discriminators on the wire, so
/// the frontend's pattern matching stays consistent across providers.
fn to_snake_case(s: &str) -> String {
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

    #[test]
    fn snake_case_examples() {
        assert_eq!(to_snake_case("PreToolUse"), "pre_tool_use");
        assert_eq!(to_snake_case("Stop"), "stop");
        assert_eq!(to_snake_case("SessionStart"), "session_start");
        assert_eq!(to_snake_case("unknown"), "unknown");
    }

    #[test]
    fn hooks_json_is_valid() {
        let s = build_hooks_json("/usr/local/bin/neige-codex-bridge");
        let v: Value = serde_json::from_str(&s).expect("valid JSON");
        assert!(v["hooks"]["PreToolUse"].is_array());
    }
}
