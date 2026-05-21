//! `/api/cards/:id/codex` — bind a `kind == "codex"` card to a live
//! interactive Codex CLI running inside a PTY.
//!
//! ## Why a PTY
//!
//! Earlier iterations spawned `codex exec <prompt>` headless and listened
//! to hook events for signal. Hook events were genuinely useful (and still
//! are — they show up as a status-bar feed in the UI), but the headless
//! `exec` mode hides the actual TUI, makes it impossible to drive codex
//! interactively, and — worst of all — appears to "hang" the moment codex
//! can't reach the model API. The user can't see the codex error, so the
//! card just sits there with no signal.
//!
//! By spawning interactive codex through the same PTY infrastructure the
//! Terminal card uses (xterm.js ↔ `calm-session-daemon` ↔ codex CLI), we
//! get the full TUI in the browser and the user can both drive the agent
//! and see when something's wrong with the network / auth / API.
//!
//! ## Flow
//!
//! 1. Validate the card exists and is `kind == "codex"`.
//! 2. `mktemp -d` a per-spawn `CODEX_HOME`. **Seed it** from `~/.codex`
//!    (auth.json / config.toml carry over) and overwrite `hooks.json` to
//!    point every event at our bridge.
//! 3. Create a `Terminal` row whose `program = "codex"`, `cwd =
//!    <user-supplied or $HOME>`, and `env` carries `CODEX_HOME`,
//!    `NEIGE_CARD_ID`, `NEIGE_CALM_BASE_URL`. The daemon will forward all
//!    env to the PTY child (see `crates/calm-session/src/bin/daemon.rs`).
//! 4. Spawn `calm-session-daemon` via `routes::terminal::spawn_daemon_for`.
//! 5. Patch the Card.payload with `terminal_id` so the frontend can attach
//!    the xterm via `/api/terminals/:id`.
//!
//! Hook events stay on the WS event bus (`card:<card_id>` → `codex.hook`)
//! exactly as before — the codex CLI runs the bridge on every hook via the
//! `hooks.json` we seed.
//!
//! ## Tempdir lifetime
//!
//! The `CODEX_HOME` tempdir is intentionally leaked (`TempDir::keep()`). The
//! kernel has no "agent died" hook on the codex card side, so there's no
//! good signal to clean up on. Leaking matches the prior behavior — the
//! tempdir survives until the next reboot's `/tmp` cleaner. Acceptable
//! for a per-card directory with auth.json + config.toml + hooks.json.
//!
//! ## Hook bridge
//!
//! Internal ingest is at `POST /internal/codex/hook?card_id=<id>`. The
//! bridge binary (`neige-codex-bridge`) is invoked by codex on every hook
//! with stdin = a single JSON object; the bridge POSTs it here and we
//! tag it with the card id and emit `Event::CodexHook` on the bus.

use crate::actor::Actor;
use crate::db::sqlite::card_update_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{Card, CardPatch, NewTerminal};
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_daemon_for;
use crate::state::AppState;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path as StdPath;
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
    /// Working directory codex runs in. Defaults to `$HOME` if empty.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Reserved field — interactive codex has slash-commands for the
    /// prompt; the API still accepts it for forward / backward
    /// compatibility but does nothing with it. Kept so older clients keep
    /// generating valid OpenAPI requests.
    #[serde(default)]
    pub initial_prompt: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/cards/{card_id}/codex",
    tag = "codex",
    params(("card_id" = String, Path, description = "Card id (must be a codex card)")),
    request_body(content = NewCodexBody, description = "Codex spawn parameters"),
    responses(
        (status = 202, description = "Codex spawned; hook events stream over WS, TUI runs in the card's PTY", body = Card),
        (status = 400, description = "Card is not a codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Spawn failed", body = ErrorBody),
    ),
)]
pub(crate) async fn create_codex(
    State(s): State<AppState>,
    actor: Actor,
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

    let card = spawn_codex_for(&s, &actor, &card, &p).await?;
    Ok((StatusCode::ACCEPTED, Json(card)))
}

/// Persist a per-card `CODEX_HOME` under `data_dir/codex-homes/<card_id>/`
/// (lives in the bind-mounted `$HOME`, so it survives container restarts),
/// seed it from `$HOME/.codex` on first creation, write our hooks.json,
/// create a Terminal row, and spawn the session daemon running interactive
/// `codex`. Returns the updated Card with `payload.terminal_id` set so the
/// frontend can attach the xterm.
async fn spawn_codex_for(
    s: &AppState,
    actor: &Actor,
    card: &Card,
    p: &NewCodexBody,
) -> Result<Card> {
    // 1. Stable per-card CODEX_HOME. Keying on card_id means daemon
    //    revives after a container restart see the same auth.json / state
    //    that codex wrote last time — the old `/tmp/`-keyed tempdir was
    //    wiped by docker, leaving the daemon stuck in a respawn loop.
    let codex_home = s.codex.codex_homes_dir.join(&card.id);
    let is_fresh = !codex_home.exists();
    std::fs::create_dir_all(&codex_home).map_err(|e| {
        CalmError::Internal(format!("mkdir codex_home {}: {e}", codex_home.display()))
    })?;

    // 2. Seed from $HOME/.codex on first creation only. Re-spawns after a
    //    restart find the dir already populated with codex's accumulated
    //    state (auth.json, history.jsonl, sessions/) — re-copying would
    //    clobber that with the user's pristine host config.
    if is_fresh
        && let Some(src) = host_codex_dir()
        && src.exists()
        && let Err(e) = copy_dir_recursive(&src, &codex_home)
    {
        tracing::warn!(error = %e, src = %src.display(), "codex seed copy failed; continuing without it");
    }

    // 3. Always (re)write hooks.json — even if the seed brought one in, or
    //    a previous spawn wrote one with a stale bridge path. Cheap to
    //    overwrite and ensures upgrades pick up the new path.
    let bridge_path = s.codex.bridge_bin.to_string_lossy().to_string();
    let hooks_json = build_hooks_json(&bridge_path);
    let hooks_path = codex_home.join("hooks.json");
    std::fs::write(&hooks_path, hooks_json)
        .map_err(|e| CalmError::Internal(format!("write hooks.json: {e}")))?;

    // 4. Resolve cwd & assemble env that the daemon will forward to the
    //    PTY child. Interactive `codex` (no args) boots into its TUI.
    let cwd = p
        .cwd
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(default_cwd);

    let codex_home_path = codex_home.to_string_lossy().to_string();

    // Pull the user's Settings snapshot. The Settings page (`/settings`)
    // owns these values; for proxy fields we **only** inject the env var
    // when the user actually has a non-empty override. Without an
    // override, we leave the env alone so the daemon's child process
    // inherits whatever proxy the container already exports (e.g. the
    // compose-supplied `HTTP_PROXY=http://127.0.0.1:10809`). Explicitly
    // setting an empty string here would *clear* the container default,
    // which is the opposite of what the user expects.
    let settings = load_settings(s.repo.as_ref()).await?;

    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "CODEX_HOME".to_string(),
        serde_json::Value::String(codex_home_path.clone()),
    );
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        serde_json::Value::String(card.id.clone()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        serde_json::Value::String(s.codex.ingest_url.clone()),
    );
    if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
        // Set both lowercase and uppercase — codex (and the OpenAI client
        // it links) reads `HTTPS_PROXY` / `HTTP_PROXY` (uppercase), but
        // most reqwest-based tools also honor lowercase. Cheap to write
        // both; matches what the container env already does.
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

    // 5. Persist the Terminal row.
    let program = "codex".to_string();
    let term = s
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: program.clone(),
            cwd: cwd.clone(),
            env: env.clone(),
        })
        .await?;

    // 6. Spawn the session daemon for this terminal.
    spawn_daemon_for(s, &term, &program, &cwd, &env).await?;

    // 7. Stamp the card payload so the frontend's `fromKernel` picks up the
    //    terminal_id and renders xterm. We merge into any existing payload
    //    so we don't clobber fields a future caller might add.
    let mut payload = card.payload.clone();
    if !payload.is_object() {
        payload = serde_json::json!({});
    }
    // Tier A persistence contract: kernel-owned card payloads carry an
    // explicit per-kind `schemaVersion`. See `docs/upgrade-stability.md`.
    payload["schemaVersion"] = serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION);
    payload["terminal_id"] = serde_json::Value::String(term.id.clone());
    if !cwd.is_empty() {
        payload["cwd"] = serde_json::Value::String(cwd.clone());
    }
    let updated = stamp_codex_terminal_payload(s, actor, card, payload).await?;

    tracing::info!(
        card_id = %card.id,
        terminal_id = %term.id,
        cwd = %cwd,
        "spawned interactive codex"
    );

    // initial_prompt is intentionally ignored — interactive codex uses its
    // own slash-command UX for input. We log it once for observability so
    // older clients pushing prompts don't silently lose anything.
    if let Some(ip) = p.initial_prompt.as_deref().filter(|s| !s.trim().is_empty()) {
        tracing::debug!(card_id = %card.id, initial_prompt = %ip, "initial_prompt ignored in interactive mode");
    }

    Ok(updated)
}

async fn stamp_codex_terminal_payload(
    s: &AppState,
    actor: &Actor,
    card: &Card,
    payload: Value,
) -> Result<Card> {
    let id = card.id.clone();
    let (updated, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.as_str(),
        None,
        &s.events,
        move |tx| {
            Box::pin(async move {
                let updated = card_update_tx(
                    tx,
                    &id,
                    CardPatch {
                        kind: None,
                        sort: None,
                        payload: Some(payload),
                    },
                )
                .await?;
                Ok((updated.clone(), Event::CardUpdated(updated)))
            })
        },
    )
    .await?;
    Ok(updated)
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: String,
}

/// Loopback-only ingest. The bridge subprocess POSTs the raw codex hook
/// payload here; we extract `hook_event_name`, tag it, and emit on the
/// bus.
///
/// Scope A — codex hook events flow through the sync engine's pure-event
/// log (`Repo::log_pure_event`) so the wire envelope carries an `_id`
/// the same way entity-write events do. The events row records every
/// hook payload verbatim; that's intentional — codex card UIs are
/// append-only ephemeral on the frontend, but the persistent event log
/// is the audit/replay store the design doc §2.3 calls out.
///
/// Scope β — the actor is now declarative: the codex bridge stamps
/// `X-Calm-Actor: ai:codex` on every POST and the `actor_middleware`
/// validates + injects an `Actor`. Pre-β this handler hardcoded `"kernel"`,
/// which was wrong on two counts: codex's lifecycle signal is an *AI*
/// write, not a server-internal one, and the audit log conflated the two.
///
/// Default-actor decision: we deliberately keep the middleware's `"user"`
/// fallback for this route. An older bridge with no header is the only
/// way to hit it, and tagging those hooks as `"user"` is honest — we
/// don't actually know it was codex. The fix is to redeploy the bridge,
/// not to silently re-attribute. (Overriding the default here would also
/// require the middleware to admit `kernel`/`ai:codex` from this path,
/// which conflicts with its "reserved namespace" gate.)
pub(crate) async fn ingest_hook(
    State(s): State<AppState>,
    actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("hook.codex.{}", to_snake_case(event_name));

    s.repo
        .log_pure_event(
            actor.as_str(),
            None,
            &s.events,
            Event::CodexHook {
                card_id: q.card_id,
                kind,
                payload,
            },
        )
        .await?;
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
    use crate::db::RepoSyncDomainRaw;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::model::{NewCard, NewCove, NewWave};
    use crate::plugin_host::{PluginHost, PluginRegistry};
    use crate::state::{CodexClient, DaemonClient};
    use std::sync::Arc;

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

    #[tokio::test]
    async fn codex_terminal_payload_stamp_persists_and_broadcasts_card_updated() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let events = EventBus::new();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
        ));
        let state = AppState::from_parts(
            repo.clone(),
            events.clone(),
            Arc::new(DaemonClient::new_stub()),
            plugin,
            Arc::new(CodexClient::new_stub()),
        );

        let cove = repo
            .cove_create(NewCove {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "w".into(),
                sort: None,
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id,
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({ "existing": true }),
            })
            .await
            .unwrap();
        let mut rx = events.subscribe();

        let payload = serde_json::json!({
            "existing": true,
            "terminal_id": "term_1",
            "cwd": "/workspace",
        });
        let actor = Actor("user".to_string());
        let updated = stamp_codex_terminal_payload(&state, &actor, &card, payload)
            .await
            .unwrap();

        assert_eq!(updated.payload["terminal_id"], "term_1");
        assert_eq!(updated.payload["cwd"], "/workspace");

        let env = rx.recv().await.expect("card.updated broadcast");
        assert!(env.id > 0, "expected real events.id");
        match env.event {
            Event::CardUpdated(updated_event) => {
                assert_eq!(updated_event.id, card.id);
                assert_eq!(updated_event.payload["terminal_id"], "term_1");
            }
            other => panic!("expected CardUpdated, got {other:?}"),
        }

        let row: (String, String) = sqlx::query_as("SELECT kind, actor FROM events WHERE id = ?1")
            .bind(env.id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(row, ("card.updated".into(), "user".into()));
    }
}
