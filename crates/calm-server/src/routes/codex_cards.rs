//! `POST /api/waves/:wave_id/codex-cards` — atomic codex-card creation.
//!
//! Structural twin of `routes/terminal_cards.rs` for the codex flow (#117).
//! Collapses what used to be a 2-step recipe — `POST .../cards` (kind=codex,
//! empty payload) followed by `POST /api/cards/:id/codex` (spawn PTY +
//! stamp `terminal_id`) — into a single endpoint:
//!
//! 1. Inside one DB transaction, `card_with_codex_create_tx` writes both
//!    the `codex`-kind card AND the linked `terminal` row, stamping
//!    `{schemaVersion, terminal_id, cwd?}` onto the card payload. The
//!    transaction also persists the `card.added` event with the final
//!    payload, so a single broadcast carries the fully-formed card to
//!    peers — no `card.updated` follow-up, no intermediate
//!    `payload=null` flash for the renderer's "Codex is starting…"
//!    placeholder to react to.
//! 2. After commit, the handler seeds the per-card `CODEX_HOME`, writes
//!    `hooks.json`, and spawns `calm-session-daemon` via the same
//!    `spawn_daemon_for` helper the terminal-card endpoint uses. A
//!    daemon-spawn failure returns 500 to the client but does NOT roll
//!    back the persisted rows: the orphan-terminal sweeper reaps them
//!    within ~60s.
//!
//! Why a pre-minted card_id (design option C)? The `CODEX_HOME` path is
//! `<codex_homes_dir>/<card_id>/` — keyed on the card id so the daemon
//! sees the same auth.json / state across container restarts. Pre-minting
//! the id lets us derive that path *before* the row hits the DB and
//! propagate it into the env map without a post-commit "stamp env" round
//! trip. The seeding+hooks I/O still happens after commit (outside the
//! transaction) because copying `$HOME/.codex` shouldn't hold a write
//! txn open.

use crate::actor::Actor;
use crate::db::sqlite::card_with_codex_create_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{Card, new_id};
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_daemon_for;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use std::path::Path as StdPath;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/waves/{wave_id}/codex-cards", post(create_codex_card))
}

/// Body for `POST /api/waves/:wave_id/codex-cards`.
///
/// Deliberately omits `kind` (always `"codex"`) and `payload` (the kernel
/// stamps `{schemaVersion, terminal_id, cwd?}` itself). Empty `cwd` falls
/// back to `$HOME` then the server's cwd. `initial_prompt` is accepted for
/// forward-/backward-compatibility with older clients but ignored —
/// interactive codex uses its own slash-command UX for input.
#[derive(Deserialize, Debug, Default, ToSchema)]
pub struct NewCodexCardBody {
    /// Sort order within the wave. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
    /// Working directory codex runs in. Empty string or missing → `$HOME`
    /// (then `cwd` of server).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Reserved field — accepted for compat; interactive codex uses its
    /// own slash-command UX for input. Logged at `debug` only when non-empty.
    #[serde(default)]
    pub initial_prompt: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/codex-cards",
    tag = "codex",
    params(("wave_id" = String, Path, description = "Wave id to create the codex card under")),
    request_body(content = NewCodexCardBody, description = "Optional body — empty means use defaults"),
    responses(
        (status = 201, description = "Card + linked terminal created atomically; codex daemon spawned", body = Card),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed (rows are persisted; sweeper reaps within ~60s)", body = ErrorBody),
    ),
)]
pub(crate) async fn create_codex_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(wave_id): Path<String>,
    body: Option<Json<NewCodexCardBody>>,
) -> Result<(StatusCode, Json<Card>)> {
    let Json(p) = body.unwrap_or_default();

    // 1. Parent wave must exist. Surfaces as 404 *before* we open the
    //    transaction — same shape as the terminal-card route. The
    //    `card_with_codex_create_tx` helper would surface a foreign-key
    //    failure as 500 (Internal) at txn commit which is less informative
    //    than this explicit pre-check.
    if s.repo.wave_get(&wave_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }

    // 2. Pre-mint the card id so we can derive `CODEX_HOME` (keyed on
    //    card id, see module-level doc) before the row hits the DB.
    let card_id = new_id();

    // 3. Resolve cwd — empty / missing falls back to `$HOME`.
    let cwd = p
        .cwd
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(default_cwd);

    // 4. Assemble the env map the daemon will forward to the PTY child:
    //    CODEX_HOME / NEIGE_CARD_ID / NEIGE_CALM_BASE_URL plus proxy vars
    //    pulled from `load_settings`. Only inject HTTP(S)_PROXY when the
    //    user has a non-empty override — empty would *clear* the container
    //    default which is the opposite of what the user expects.
    let codex_home = s.codex.codex_homes_dir.join(&card_id);
    let codex_home_path = codex_home.to_string_lossy().to_string();
    let settings = load_settings(s.repo.as_ref()).await?;
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "CODEX_HOME".to_string(),
        serde_json::Value::String(codex_home_path.clone()),
    );
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        serde_json::Value::String(card_id.clone()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        serde_json::Value::String(s.codex.ingest_url.clone()),
    );
    if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
        // codex (and the OpenAI client it links) reads `HTTPS_PROXY` /
        // `HTTP_PROXY` (uppercase); most reqwest-based tools also honor
        // lowercase. Cheap to write both.
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

    // 5. Single transaction: card row + terminal row + payload link + event.
    //    A single `card.added` envelope carries the final-state card to
    //    all peers — no intermediate `payload=null` snapshot, no follow-up
    //    `card.updated`.
    let sort = p.sort;
    let card_id_for_tx = card_id.clone();
    let cwd_for_tx = cwd.clone();
    let env_for_tx = env.clone();
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                let (card, _term) = card_with_codex_create_tx(
                    tx,
                    card_id_for_tx,
                    wave_id,
                    sort,
                    cwd_for_tx,
                    env_for_tx,
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await?;

    // initial_prompt is intentionally ignored — interactive codex uses its
    // own slash-command UX. Log once at debug for observability so older
    // clients pushing prompts don't silently lose anything.
    if let Some(ip) = p.initial_prompt.as_deref().filter(|s| !s.trim().is_empty()) {
        tracing::debug!(card_id = %card.id, initial_prompt = %ip, "initial_prompt ignored in interactive mode");
    }

    // 6. Post-commit (out of the transaction): seed CODEX_HOME and write
    //    hooks.json. Copying `$HOME/.codex` shouldn't hold a write txn
    //    open, and the daemon doesn't read these files until it spawns,
    //    so doing the I/O here is safe.
    let is_fresh = !codex_home.exists();
    std::fs::create_dir_all(&codex_home).map_err(|e| {
        CalmError::Internal(format!("mkdir codex_home {}: {e}", codex_home.display()))
    })?;
    if is_fresh
        && let Some(src) = host_codex_dir()
        && src.exists()
        && let Err(e) = copy_dir_recursive(&src, &codex_home)
    {
        tracing::warn!(error = %e, src = %src.display(), "codex seed copy failed; continuing without it");
    }
    // Always (re)write hooks.json — even if the seed brought one in, or a
    // previous spawn wrote one with a stale bridge path. Cheap to overwrite
    // and ensures upgrades pick up the new path.
    let bridge_path = s.codex.bridge_bin.to_string_lossy().to_string();
    let hooks_json = build_hooks_json(&bridge_path);
    let hooks_path = codex_home.join("hooks.json");
    std::fs::write(&hooks_path, hooks_json)
        .map_err(|e| CalmError::Internal(format!("write hooks.json: {e}")))?;

    // 7. Fetch the persisted terminal row so we can hand it to
    //    `spawn_daemon_for`. Guaranteed to exist: the transaction above
    //    committed both card and terminal as one unit.
    let term = s
        .repo
        .terminal_get_by_card(&card.id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "terminal vanished after commit for card {}",
                card.id
            ))
        })?;

    // 8. Spawn the daemon. On failure we deliberately do NOT roll back
    //    the persisted rows — the orphan-terminal sweeper handles cleanup
    //    within its grace window. Matches the prior endpoint's semantics:
    //    a 500 tells the client the spawn failed, but the card/terminal
    //    pair is still in the DB until the sweeper runs.
    spawn_daemon_for(&s, &term, "codex", &cwd, &env).await?;

    tracing::info!(
        card_id = %card.id,
        terminal_id = %term.id,
        cwd = %cwd,
        "spawned interactive codex"
    );

    Ok((StatusCode::CREATED, Json(card)))
}

// ---------------------------------------------------------------------------
// Helpers — moved here from `routes/codex.rs` along with the endpoint they
// support. The remaining `routes/codex.rs` file keeps only the hook-ingest
// loopback route + its query-param struct.
// ---------------------------------------------------------------------------

/// `~/.codex` on the host — visible inside the docker container thanks to
/// the `${HOME}:${HOME}` bind mount in docker-compose.yml.
pub(crate) fn host_codex_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex"))
}

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

/// Recursively copy `src` to `dst`. Minimal walker — no symlink chasing,
/// no perm propagation beyond what `std::fs::copy` does. Enough to seed
/// `auth.json` / `config.toml` / `models_cache.json` and any sibling
/// dirs codex caches there.
pub(crate) fn copy_dir_recursive(src: &StdPath, dst: &StdPath) -> std::io::Result<()> {
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
        // inside the per-card dir.
    }
    Ok(())
}

pub(crate) fn build_hooks_json(bridge: &str) -> String {
    // Bridge command — the binary path resolved by `state::CodexClient`.
    // Codex spec: each hook entry is `{"type":"command", "command":"<argv>"}`.
    // We rely on PATH lookup if `bridge` is a bare name.
    let cmd =
        serde_json::to_string(bridge).unwrap_or_else(|_| String::from("\"neige-codex-bridge\""));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn hooks_json_is_valid() {
        let s = build_hooks_json("/usr/local/bin/neige-codex-bridge");
        let v: Value = serde_json::from_str(&s).expect("valid JSON");
        assert!(v["hooks"]["PreToolUse"].is_array());
    }
}
