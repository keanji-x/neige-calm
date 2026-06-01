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
//! 2. After commit, the handler starts the terminal renderer via the same
//!    `spawn_terminal_for` helper the terminal-card endpoint uses. Empty
//!    prompt cards still seed per-card `CODEX_HOME` before the legacy bare
//!    `codex` spawn. Non-empty prompt cards use the shared app-server when
//!    explicitly enabled for prompt cards: the kernel creates a thread,
//!    sends turn #1, waits for the first lifecycle notification, and spawns
//!    a remote `codex resume` TUI.
//!    A renderer-start failure returns 500 to the client but does NOT roll
//!    back the persisted rows: the orphan-terminal sweeper reaps them within
//!    ~60s.
//!
//! Why a pre-minted card_id (design option C)? The `CODEX_HOME` path is
//! `<codex_homes_dir>/<card_id>/` — keyed on the card id so the renderer
//! sees the same auth.json / state across container restarts. Pre-minting
//! the id lets us derive that path *before* the row hits the DB and
//! propagate it into the env map without a post-commit "stamp env" round
//! trip. The seeding I/O still happens after commit (outside the
//! transaction) because copying `$HOME/.codex` shouldn't hold a write
//! txn open.

use crate::actor::Actor;
use crate::codex_appserver::{InputItem, Notification};
use crate::db::sqlite::{card_codex_thread_upsert_tx, card_update_tx, card_with_codex_create_tx};
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{Card, CardPatch, CardRole, new_id};
use crate::routes::cards::card_scope;
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
use std::path::Path as StdPath;
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
/// `prompt` is the hands-free entry point: when non-empty and the shared
/// codex prompt-card path is enabled, the kernel starts a shared thread,
/// persists its id on both the payload and `card_codex_threads`, sends the
/// prompt via `turn/start`, waits for `turn/started` or `turn/completed`,
/// and starts the TUI as `codex resume <thread_id> --remote unix://...`.
///
/// If the shared-daemon rollback flag is disabled, a non-empty prompt stays
/// on the legacy path: pass it to codex CLI as positional `[PROMPT]`, write
/// per-card `config.toml`, and let `codex_auto_submit` inject `\r`.
///
/// Empty / absent `prompt` reverts to the user-initiated flow: codex
/// boots, the composer is empty, the user types and hits Enter.
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
pub(crate) async fn create_codex_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(wave_id): Path<String>,
    Json(p): Json<NewCodexCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    // 1. Parent wave must exist. Surfaces as 404 *before* we open the
    //    transaction — same shape as the terminal-card route. The
    //    `card_with_codex_create_tx` helper would surface a foreign-key
    //    failure as 500 (Internal) at txn commit which is less informative
    //    than this explicit pre-check.
    if s.repo.wave_get(&wave_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }

    // Validate `cwd` at the request boundary: a value containing ASCII
    // control characters (`\n`, `\r`, `\t`, `\0`, `\x7f`, ...) would
    // produce TOML-spec-invalid output when `build_codex_config_toml`
    // hand-escapes it into a basic string, and codex's config parser
    // would crash at spawn time. Reject up front with 400 so the caller
    // gets a deterministic, debuggable signal instead of a daemon spawn
    // failure deep in the pipeline. Only validates when the caller
    // supplied a value — `None` / empty string still falls back to
    // `default_cwd()` below.
    if let Some(raw) = p.cwd.as_deref()
        && raw.chars().any(|c| c.is_ascii_control())
    {
        return Err(CalmError::BadRequest(
            "cwd must not contain ASCII control characters".into(),
        ));
    }
    let icon_bg = normalize_optional_css_color(p.icon_bg.as_deref(), "icon_bg")?;
    let icon_fg = normalize_optional_css_color(p.icon_fg.as_deref(), "icon_fg")?;

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

    // Normalize prompt up-front: trim + non-empty filter. This is the
    // single source of truth for "is this a hands-free spawn?" — the
    // payload stamp, the config.toml write, and the codex argv all key
    // off the same Option<String>. None / empty → user-initiated flow.
    let prompt = p
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let shared_prompt_requested = prompt.is_some() && s.shared_codex_prompt_cards_enabled;
    let use_shared_prompt_path = shared_prompt_requested && s.shared_codex_appserver.is_running();

    // 4. Assemble the env map the daemon will forward to the PTY child:
    //    CODEX_HOME / NEIGE_CARD_ID / NEIGE_CALM_BASE_URL plus proxy vars
    //    pulled from `load_settings`. Only inject HTTP(S)_PROXY when the
    //    user has a non-empty override — empty would *clear* the container
    //    default which is the opposite of what the user expects.
    let codex_home = s.codex.codex_homes_dir.join(&card_id);
    let codex_home_path = if use_shared_prompt_path {
        s.codex.codex_home_dir().to_string_lossy().to_string()
    } else {
        codex_home.to_string_lossy().to_string()
    };
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
    let prompt_for_tx = prompt.clone();
    let icon_bg_for_tx = icon_bg.clone();
    let icon_fg_for_tx = icon_fg.clone();
    // #177 — host browser's theme is written onto the terminal row in
    // the same tx that mints the card. The spawn helper below reads
    // `term.theme_fg/bg` directly when stamping the daemon argv, so
    // any spawn for this row (initial, auto-revive, dispatcher) gets
    // identical `--terminal-fg/-bg` values by construction.
    let theme_for_tx = p.theme;
    // Pre-built `EventScope::Card` — `card_id` is pre-minted on this
    // endpoint (see module-level doc), so the scope is fully determined
    // before the txn opens.
    let scope = card_scope(
        s.repo.as_ref(),
        card_id.clone().into(),
        wave_id.clone().into(),
    )
    .await?;
    let wave_id_for_tx = wave_id.clone();
    let cache_for_tx = s.card_role_cache.clone();
    let (mut card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                let (card, _term, _token) = card_with_codex_create_tx(
                    tx,
                    card_id_for_tx,
                    wave_id_for_tx.into(),
                    sort,
                    cwd_for_tx,
                    env_for_tx,
                    prompt_for_tx,
                    icon_bg_for_tx,
                    icon_fg_for_tx,
                    // User-facing codex cards stay Plain. The spec
                    // role is exclusively minted by the wave-create
                    // route (PR6), and the dispatcher mints Worker
                    // role through the standalone card_create path.
                    // PR7a: Plain cards skip token minting, so the
                    // third return slot is always `None` here — we
                    // discard it explicitly to make the contract
                    // obvious at the call site.
                    crate::model::CardRole::Plain,
                    // Issue #229 PR A — user-facing codex cards are
                    // user-deletable. Spec cards take the `false`
                    // route via routes/waves.rs.
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

    // 6. Fetch the persisted terminal row so we can hand it to
    //    `spawn_terminal_for`. Guaranteed to exist: the transaction above
    //    committed both card and terminal as one unit.
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

    if shared_prompt_requested && !use_shared_prompt_path {
        tracing::warn!(
            target: "shared_codex_daemon::fallback_to_legacy",
            card_id = %card.id,
            reason = "shared codex app-server not running",
            "PR3c degraded fallback: legacy per-card path used"
        );
    }

    let command_line = if let Some(prompt_text) = prompt.as_deref() {
        if use_shared_prompt_path {
            // PR5 new path: non-empty user prompt cards run turn #1 on the
            // server-wide shared app-server, then the PTY TUI attaches to
            // that durable thread via `codex resume ... --remote`.
            let mut notifs = s.shared_codex_appserver.subscribe_notifications();
            let thread_id = match s
                .shared_codex_appserver
                .thread_start_for_card(
                    card.id.as_ref(),
                    CardRole::Plain,
                    Some(wave_id.as_str()),
                    crate::shared_codex_appserver::SharedThreadStartParams {
                        cwd: cwd.clone(),
                        approval_policy: "never".into(),
                        sandbox_mode: "workspace-write".into(),
                        developer_instructions: None,
                    },
                )
                .await
            {
                Ok(thread_id) => thread_id,
                Err(e) => {
                    tracing::error!(
                        target = "shared_codex_daemon::user_prompt_card_start_failed",
                        card_id = %card.id,
                        error = %e,
                    );
                    return Err(e);
                }
            };

            let mut payload = card.payload.clone();
            let Some(map) = payload.as_object_mut() else {
                return Err(CalmError::Internal(format!(
                    "codex card {} payload is not a JSON object; cannot persist codex_thread_id",
                    card.id
                )));
            };
            map.insert(
                "codex_thread_id".into(),
                serde_json::Value::String(thread_id.clone()),
            );

            let scope = card_scope(s.repo.as_ref(), card.id.clone(), card.wave_id.clone()).await?;
            let card_id_for_tx = card.id.to_string();
            let thread_id_for_tx = thread_id.clone();
            let wave_id_for_tx = wave_id.clone();
            let payload_for_tx = payload;
            let (updated, _id) = write_with_event_typed(
                s.repo.as_ref(),
                actor.to_actor_id(),
                scope,
                None,
                &s.events,
                &s.card_role_cache,
                &s.wave_cove_cache,
                move |tx| {
                    Box::pin(async move {
                        card_codex_thread_upsert_tx(
                            tx,
                            &card_id_for_tx,
                            &thread_id_for_tx,
                            CardRole::Plain,
                            Some(wave_id_for_tx.as_str()),
                        )
                        .await?;
                        let card = card_update_tx(
                            tx,
                            &card_id_for_tx,
                            CardPatch {
                                kind: None,
                                sort: None,
                                payload: Some(payload_for_tx),
                                deletable: None,
                            },
                        )
                        .await?;
                        Ok((card.clone(), Event::CardUpdated(card)))
                    })
                },
            )
            .await?;
            card = updated;

            s.shared_codex_appserver
                .turn_start(&thread_id, vec![InputItem::text(prompt_text)])
                .await?;
            await_shared_initial_turn_lifecycle(&mut notifs, &thread_id).await?;

            tracing::info!(
                target = "shared_codex_daemon::user_prompt_card_started",
                card_id = %card.id,
                wave_id = %wave_id,
                thread_id = %thread_id,
                prompt_len = prompt_text.len(),
            );
            format!(
                "codex resume {} --remote {}",
                shell_single_quote(&thread_id),
                shell_single_quote(&s.shared_codex_appserver.remote_uri()),
            )
        } else {
            // Rollback knob path: PR5 shared daemon is disabled, so a
            // non-empty prompt keeps the legacy argv/config/auto-submit flow.
            seed_legacy_codex_home(&codex_home, Some(&cwd))?;
            format!("codex {}", shell_single_quote(prompt_text))
        }
    } else {
        // PR6 pending path: empty prompt cards still use the legacy bare
        // codex spawn with a per-card CODEX_HOME seeded from the host.
        seed_legacy_codex_home(&codex_home, None)?;
        tracing::info!(
            target = "codex_auto_submit::spawned",
            card_id = %card.id,
            source = "empty-codex-card",
            "legacy empty codex card spawned"
        );
        "codex".to_string()
    };

    // 7. Spawn the daemon. On failure we deliberately do NOT roll back
    //    the persisted rows — the orphan-terminal sweeper handles cleanup
    //    within its grace window. Matches the prior endpoint's semantics:
    //    a 500 tells the client the spawn failed, but the card/terminal
    //    pair is still in the DB until the sweeper runs.
    spawn_terminal_for(&s, &term, &command_line, &cwd, &env).await?;

    tracing::info!(
        card_id = %card.id,
        terminal_id = %term.id,
        cwd = %cwd,
        hands_free = prompt.is_some(),
        shared_prompt_thread = use_shared_prompt_path,
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

fn seed_legacy_codex_home(codex_home: &StdPath, config_cwd: Option<&str>) -> Result<()> {
    let is_fresh = !codex_home.exists();
    std::fs::create_dir_all(codex_home).map_err(|e| {
        CalmError::Internal(format!("mkdir codex_home {}: {e}", codex_home.display()))
    })?;
    if is_fresh
        && let Some(src) = host_codex_dir()
        && src.exists()
        && let Err(e) = copy_dir_recursive(&src, codex_home)
    {
        tracing::warn!(error = %e, src = %src.display(), "codex seed copy failed; continuing without it");
    }

    if let Some(cwd) = config_cwd {
        let cfg_path = codex_home.join("config.toml");
        let cfg_text = build_codex_config_toml(cwd);
        std::fs::write(&cfg_path, cfg_text)
            .map_err(|e| CalmError::Internal(format!("write config.toml: {e}")))?;
    }
    Ok(())
}

async fn await_shared_initial_turn_lifecycle(
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

/// Per-spawn `$CODEX_HOME/config.toml` body. Silences the three
/// first-run dialogs that would otherwise gate composer mount for a
/// fresh CODEX_HOME:
///
///   - `approval_policy = "never"` — don't ask before each command.
///   - `sandbox_mode = "workspace-write"` — confirms the sandbox
///     posture so codex doesn't prompt for it on first run.
///   - `[projects."<cwd>"] trust_level = "trusted"` — pre-trusts the
///     spawn cwd so the "Trust this directory?" modal doesn't appear
///     before the composer is mounted; the auto-submitted `\r` would
///     otherwise land on that modal.
///
/// Deliberately omits any `[mcp_servers.*]` blocks. MCP server config
/// is seeded from the host `$HOME/.codex/config.toml` via
/// `copy_dir_recursive`; emitting one here would shadow the user's
/// real config in this per-card CODEX_HOME. The
/// `config_toml_has_no_mcp_servers_block` regression test below guards
/// this.
/// PR1 (#410): legacy per-card path. Shared replacement is in
/// `crate::shared_codex_home::SharedCodexHome::ensure_config_for_cwd`,
/// but callers are not switched until later #410 PRs.
pub(crate) fn build_codex_config_toml(cwd: &str) -> String {
    // We hand-write the TOML (no `toml` crate in the workspace) — the
    // payload is small enough to be readable, and the only field that
    // can contain anything wild is `cwd`. TOML basic strings allow
    // most characters but require escaping `"` and `\`. Backslash
    // shows up on Windows-style paths (which the codex container does
    // not see, but the test fixture path is a tempdir so we keep the
    // escape minimal-but-correct).
    let escaped_cwd = cwd.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "# Generated by neige-calm per-spawn — silences codex's first-run\n\
         # dialogs so an auto-submitted \\r lands on the composer.\n\
         approval_policy = \"never\"\n\
         sandbox_mode = \"workspace-write\"\n\
         \n\
         [projects.\"{escaped_cwd}\"]\n\
         trust_level = \"trusted\"\n"
    )
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

    #[test]
    fn config_toml_pre_trusts_cwd_and_silences_dialogs() {
        let s = build_codex_config_toml("/workspace");
        assert!(s.contains("approval_policy = \"never\""));
        assert!(s.contains("sandbox_mode = \"workspace-write\""));
        assert!(s.contains("[projects.\"/workspace\"]"));
        assert!(s.contains("trust_level = \"trusted\""));
    }

    /// Regression guard: per-spawn config.toml must NEVER contain a
    /// `[mcp_servers.*]` block. MCP config is seeded from the host
    /// `$HOME/.codex/config.toml` via `copy_dir_recursive`; emitting
    /// one here would shadow the user's real config.
    ///
    /// PR7a will flip this when `[mcp_servers.calm]` block lands for
    /// Spec/Worker cards (kernel-as-MCP-server). Plain cards (this
    /// route's product) stay free of the block — they never expose
    /// the kernel tools.
    #[test]
    fn config_toml_has_no_mcp_servers_block() {
        let s = build_codex_config_toml("/workspace");
        assert!(
            !s.contains("[mcp_servers"),
            "per-spawn config.toml must not contain mcp_servers blocks; got:\n{s}"
        );
    }

    #[test]
    fn config_toml_escapes_quoted_cwd() {
        // A cwd with a `"` would otherwise break the TOML table header.
        let s = build_codex_config_toml(r#"/work"dir"#);
        // The cwd is inside a basic string, so `"` must be `\"`.
        assert!(
            s.contains("[projects.\"/work\\\"dir\"]"),
            "expected escaped quote in projects header; got:\n{s}"
        );
    }
}
