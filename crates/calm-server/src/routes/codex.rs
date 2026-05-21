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
//! 2. Per-card `CODEX_HOME` under `data_dir/codex-homes/<card_id>/`. Seeded
//!    from `~/.codex` (auth.json / config.toml carry over) on first
//!    creation; subsequent spawns reuse whatever codex itself wrote there.
//! 3. Create a `Terminal` row whose `program = "codex"`, `cwd =
//!    <user-supplied or $HOME>`, and `env` carries `CODEX_HOME`,
//!    `NEIGE_CARD_ID`, `NEIGE_CALM_BASE_URL`. The daemon will forward all
//!    env to the PTY child (see `crates/calm-session/src/bin/daemon.rs`).
//! 4. Spawn `calm-session-daemon` via `routes::terminal::spawn_daemon_for`.
//! 5. Patch the Card.payload with `terminal_id` so the frontend can attach
//!    the xterm via `/api/terminals/:id`.
//!
//! Hook events stay on the WS event bus (`card:<card_id>` → `codex.hook`)
//! exactly as before — the codex CLI runs the bridge on every hook. The
//! hooks themselves are declared once, system-wide, in
//! `/etc/codex/requirements.toml` (see `docker/codex-requirements.toml`)
//! as policy-managed: codex's hook discovery returns `HookTrustStatus::
//! Managed` for them, so they fire automatically with no per-card
//! `hooks.json` write and no `/hooks` review-modal step.
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
    /// Optional prompt to pre-fill the TUI's composer with. When set,
    /// codex is spawned as `codex '<prompt>'` (positional `[PROMPT]`
    /// arg, shell-single-quoted). Codex 0.132's TUI stores this on the
    /// `ChatWidget` as `initial_user_message` and renders it in the
    /// composer awaiting Enter; it does NOT auto-submit on its own
    /// — pair with `auto_submit = true` for a fully hands-free spawn.
    /// `None` (the default) leaves the launch unchanged so user-created
    /// codex cards continue to land on an empty composer.
    #[serde(default)]
    pub prompt: Option<String>,
    /// When `true`, the kernel injects a single `\r` over the per-
    /// terminal daemon socket ~600 ms after the codex `session_start`
    /// hook fires, submitting whatever's currently in the composer.
    /// Combined with `prompt` this gives a fully hands-free
    /// "spawn → composer populated → submit" flow for caller-driven
    /// agent spawns. Defaults to `false` so the existing user-initiated
    /// spawn behavior (TUI lands, user types and presses Enter) is
    /// preserved.
    ///
    /// This is the only persisted bit the auto-submit subscriber reads
    /// off the card payload — see `codex_auto_submit.rs`.
    #[serde(default)]
    pub auto_submit: bool,
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
/// seed it from `$HOME/.codex` on first creation, write a per-spawn
/// `config.toml` (silences codex's first-run trust/approval/sandbox
/// modals so a `prompt`/`auto_submit` caller can run end-to-end without
/// a user keystroke), create a Terminal row, and spawn the session
/// daemon running interactive `codex`. Returns the updated Card with
/// `payload.terminal_id` set so the frontend can attach the xterm.
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

    // 3. Hooks come from `/etc/codex/requirements.toml` (bind-mounted via
    //    docker-compose) as policy-managed entries, so we no longer write
    //    a per-card `$CODEX_HOME/hooks.json`. Managed hooks fire without a
    //    `/hooks` review step; see the docker file's header for the
    //    discovery-path rationale.

    // 4. Resolve cwd & assemble env that the daemon will forward to the
    //    PTY child. Interactive `codex` (no args) boots into its TUI.
    let cwd = p
        .cwd
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(default_cwd);

    // 4a. Write per-spawn `config.toml` to silence codex's three blocking
    //     first-run dialogs (trust / approval / sandbox). Without these
    //     keys a fresh CODEX_HOME lands on the "Trust this directory?"
    //     modal *before* the TUI's composer mounts, so a caller-supplied
    //     `prompt` would never make it to the prompt area and an
    //     `auto_submit = true` `\r` injection would land on the modal
    //     instead. Writing config.toml on every spawn (not just `is_fresh`)
    //     keeps the keys aligned with the current `cwd` if a caller
    //     re-spawns into a different directory on the same card.
    let config_toml = build_config_toml(&cwd);
    let config_path = codex_home.join("config.toml");
    std::fs::write(&config_path, config_toml).map_err(|e| {
        CalmError::Internal(format!("write config.toml {}: {e}", config_path.display()))
    })?;

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
    //
    //    Codex 0.132's TUI takes a positional `[PROMPT]` argument that
    //    pre-fills the composer (does NOT auto-submit on its own — pair
    //    with `auto_submit = true` and the `codex_auto_submit`
    //    subscriber, which injects a `\r` after `session_start`). The
    //    daemon execs `/bin/sh -c <program>`, so the prompt has to be
    //    shell-quoted; `shell_single_quote` wraps in `'...'` and escapes
    //    internal `'` as `'\''`.
    let program = match p.prompt.as_deref() {
        Some(prompt) if !prompt.is_empty() => {
            format!("codex {}", shell_single_quote(prompt))
        }
        _ => "codex".to_string(),
    };
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
    // Persist `auto_submit` so the bus subscriber (`codex_auto_submit`)
    // can decide whether this card is eligible for a `\r` injection
    // when its `session_start` hook fires. Only stamp when `true` so we
    // don't pollute the payload of every user-initiated codex card with
    // a `false` we'd then have to explain in the wire docs. (`prompt`
    // is consumed by the spawn and intentionally NOT persisted — it
    // shows up in the composer immediately and is then user-owned.)
    if p.auto_submit {
        payload["auto_submit"] = serde_json::Value::Bool(true);
    }
    let updated = stamp_codex_terminal_payload(s, actor, card, payload).await?;

    tracing::info!(
        card_id = %card.id,
        terminal_id = %term.id,
        cwd = %cwd,
        prompt = p.prompt.as_deref().unwrap_or(""),
        auto_submit = p.auto_submit,
        "spawned interactive codex"
    );

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

/// Quote `s` for safe inclusion as a single argument inside a
/// POSIX-`/bin/sh -c` command line.
///
/// Strategy is the canonical single-quote wrap: every char passes through
/// uninterpreted *except* `'`, which can't appear inside a single-quoted
/// string and is escaped via the well-known close-quote-then-escaped-
/// quote-then-reopen idiom (`'\''`). The result is always safe regardless
/// of what's in `s` (backslashes, dollar signs, double quotes, newlines —
/// all literal under single quotes).
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            // Close the current single-quoted run, emit an escaped
            // literal quote, reopen the single quote.
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Escape a string for a TOML basic string. We hand-roll this rather
/// than pull a TOML serializer crate because the only thing we emit is
/// a fixed shape of keys (paths + strings the caller hands us), and the
/// escape set we actually need is small: backslash and double-quote
/// inside a `"..."` string.
fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build the per-spawn `config.toml` codex reads at startup.
///
/// Sole purpose: silence the three blocking first-run dialogs codex
/// would otherwise pop *before* its TUI composer mounts. The keys are
/// chosen for the "agent should run end-to-end without a user keystroke"
/// use case — a `prompt` + `auto_submit = true` spawn — and are
/// deliberately permissive. Callers that want a more restrictive
/// posture (e.g. require approval for shell commands) can layer their
/// own MCP config or post-spawn configuration on top; this file is the
/// *minimum* needed to unblock the spawn.
///
/// Keys, in order:
///   - `approval_policy = "never"` — skip codex's per-command approval
///     prompt.
///   - `sandbox_mode = "workspace-write"` — skip the sandbox-mode
///     first-run prompt; grant write access to the spawn cwd.
///   - `[projects."<cwd>"] trust_level = "trusted"` — skip the per-
///     directory "Trust this directory?" modal. The path MUST match the
///     cwd codex actually spawns in, which is why `build_config_toml`
///     takes it as a parameter rather than reading `$HOME` or similar.
///
/// Hooks come from `/etc/codex/requirements.toml` (the policy-managed
/// file), NOT from here. MCP servers are intentionally out of scope: any
/// caller that wants to expose tools to the agent layers its own MCP
/// block on top of this file (or in their own spawn-time write); this
/// PR's responsibility is the bare hands-free primitive.
fn build_config_toml(trust_cwd: &str) -> String {
    let mut s = String::with_capacity(192);
    s.push_str("# Generated by neige-calm at codex spawn time.\n");
    s.push_str("# Do not edit by hand — overwritten on every spawn.\n");
    s.push('\n');
    // Top-level keys MUST come before any `[table]` headers per TOML
    // spec, otherwise they'd be parsed as members of the next table.
    s.push_str("approval_policy = \"never\"\n");
    s.push_str("sandbox_mode = \"workspace-write\"\n");
    s.push('\n');
    s.push_str(&format!("[projects.{}]\n", toml_quote(trust_cwd)));
    s.push_str("trust_level = \"trusted\"\n");
    s
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
    fn shell_single_quote_round_trip_under_sh() {
        // Plain text: bare single-quoted wrap.
        assert_eq!(shell_single_quote("hello"), "'hello'");
        // Embedded single quote uses the close/escape/reopen idiom.
        assert_eq!(shell_single_quote("don't"), "'don'\\''t'");
        // Double quotes, backslashes, and dollar signs are literal under
        // single quotes — no further escaping required.
        assert_eq!(shell_single_quote(r#"a"b"#), "'a\"b'");
        assert_eq!(shell_single_quote(r"a\b"), r"'a\b'");
        assert_eq!(shell_single_quote("$HOME"), "'$HOME'");
        // Empty string still yields a syntactically valid empty arg.
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn toml_quote_escapes_backslash_and_quote() {
        assert_eq!(toml_quote("plain"), r#""plain""#);
        assert_eq!(toml_quote("a\"b"), r#""a\"b""#);
        assert_eq!(toml_quote("a\\b"), r#""a\\b""#);
    }

    #[test]
    fn config_toml_silences_first_run_dialogs() {
        let s = build_config_toml("/home/kenji");
        assert!(s.contains(r#"approval_policy = "never""#));
        assert!(s.contains(r#"sandbox_mode = "workspace-write""#));
        assert!(s.contains(r#"[projects."/home/kenji"]"#));
        assert!(s.contains(r#"trust_level = "trusted""#));
        // Top-level keys must precede the first [table] header — otherwise
        // TOML parses them as members of that table.
        let approval_idx = s.find("approval_policy = ").unwrap();
        let first_table_idx = s.find('[').unwrap();
        assert!(
            approval_idx < first_table_idx,
            "approval_policy must appear before any [table] header"
        );
    }

    #[test]
    fn config_toml_projects_table_uses_passed_cwd() {
        let s = build_config_toml("/var/lib/agent-cards/abc");
        assert!(s.contains(r#"[projects."/var/lib/agent-cards/abc"]"#));
        assert!(!s.contains(r#"[projects."/home/kenji"]"#));
    }

    #[test]
    fn config_toml_does_not_write_mcp_block() {
        // Out of scope for this PR — callers wanting MCP tools layer
        // their own config on top.
        let s = build_config_toml("/tmp/cwd");
        assert!(
            !s.contains("[mcp_servers"),
            "build_config_toml must not emit any [mcp_servers.*] block; \
             that's a caller concern. Got:\n{s}"
        );
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
