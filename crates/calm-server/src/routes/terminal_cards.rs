//! `POST /api/waves/:wave_id/terminal-cards` — atomic terminal-card creation.
//!
//! Collapses what used to be a 3-step recipe (card-add → terminal-create →
//! card-update with `terminal_id` payload) into a single endpoint:
//!
//! 1. Inside one DB transaction, `card_with_terminal_create_tx` writes both
//!    the `terminal`-kind card AND the linked `terminal` row, stamping
//!    `{schemaVersion, terminal_id}` onto the card payload. The transaction
//!    also persists the `card.added` event with the final payload, so a
//!    single broadcast carries the fully-formed card to peers — no
//!    `card.updated` follow-up, no intermediate "half-built" state for
//!    EventBridge to react to.
//! 2. After commit, the handler spawns `calm-session-daemon` via the same
//!    `spawn_terminal_for` helper the GET-side and the codex route still use.
//!    A daemon-spawn failure returns 500 to the client but does NOT roll
//!    back the persisted rows: the orphan-terminal sweeper reaps them
//!    within ~60s. This matches the prior behavior of the deleted
//!    `POST /api/cards/:id/terminal` handler.
//!
//! See #13 for the motivating problem (terminal-card create twitch caused by
//! the multi-event race) and PR1 (#107) for the DB helper this endpoint
//! consumes.

use crate::actor::Actor;
use crate::db::sqlite::card_with_terminal_create_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::model::{Card, new_id};
use crate::routes::cards::card_scope;
use crate::routes::terminal::spawn_terminal_for;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/waves/{wave_id}/terminal-cards",
        post(create_terminal_card),
    )
}

/// Body for `POST /api/waves/:wave_id/terminal-cards`.
///
/// Deliberately omits `kind` (always `"terminal"`) and `payload` (the kernel
/// stamps `{schemaVersion, terminal_id}` itself). Empty `program` falls back
/// to `$SHELL` then `/bin/sh`; empty `cwd` falls back to `$HOME` then the
/// server's cwd. `env` is merged into the daemon's environment as additional
/// vars on top of `TERM` / `COLORTERM` / inherited.
#[derive(Deserialize, Debug, ToSchema)]
pub struct NewTerminalCardBody {
    /// Sort order within the wave. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
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
    /// Host browser's current theme RGB (#177). Required — the kernel
    /// writes it onto the terminal row inside the same transaction
    /// that mints the card, and every spawn for this row reads
    /// `term.theme_fg/_bg` to stamp `--terminal-fg/-bg` daemon argv.
    pub theme: crate::routes::theme::RequestTheme,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/terminal-cards",
    tag = "terminals",
    params(("wave_id" = String, Path, description = "Wave id to create the terminal card under")),
    request_body(content = NewTerminalCardBody, description = "Body required (theme is mandatory; program/cwd/env optional)"),
    responses(
        (status = 201, description = "Card + linked terminal created atomically; daemon spawned", body = Card),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed (rows are persisted; sweeper reaps within ~60s)", body = ErrorBody),
    ),
)]
pub(crate) async fn create_terminal_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(wave_id): Path<String>,
    Json(p): Json<NewTerminalCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    // 1. Parent wave must exist. Surfaces as 404 *before* we open the
    //    transaction. The card_with_terminal_create_tx helper would surface
    //    a foreign-key failure as a 500 (Internal) at txn commit which is
    //    less informative than this explicit pre-check.
    if s.repo.wave_get(&wave_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }

    // 2. Resolve defaults the same way the prior endpoint did.
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

    // 3. Single transaction: card row + terminal row + payload link + event.
    //    A single `card.added` envelope carries the final-state card to all
    //    peers — no intermediate `payload=null` snapshot, no follow-up
    //    `card.updated` patch.
    //
    //    PR2 of #136: pre-mint the card id so `EventScope::Card { card,
    //    .. }` is determinable before the txn opens (matches the codex
    //    endpoint's pattern from #117).
    let sort = p.sort;
    let card_id = new_id();
    let program_for_tx = program.clone();
    let cwd_for_tx = cwd.clone();
    let env_for_tx = env.clone();
    // #177 — host browser's theme, written onto the terminal row in
    // the same tx alongside the card. Spawn helper reads it back to
    // stamp `--terminal-fg/-bg`.
    let theme_for_tx = p.theme;
    let scope = card_scope(
        s.repo.as_ref(),
        card_id.clone().into(),
        wave_id.clone().into(),
    )
    .await?;
    let card_id_for_tx = card_id.clone();
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
                let (card, _term) = card_with_terminal_create_tx(
                    tx,
                    card_id_for_tx,
                    wave_id_for_tx.into(),
                    sort,
                    program_for_tx,
                    cwd_for_tx,
                    env_for_tx,
                    // User-facing terminal cards stay Plain. The
                    // dispatcher's worker-terminal path passes
                    // `CardRole::Worker` directly.
                    crate::model::CardRole::Plain,
                    // Issue #229 PR A — terminal cards are
                    // user-deletable.
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

    // 4. Fetch the persisted terminal row so we can hand it to
    //    `spawn_terminal_for`. The helper stamps the daemon socket path back
    //    onto the row via `renderer setup`, so we don't need the
    //    pre-spawn snapshot — we just need its id + program/cwd/env, which
    //    the row carries. The row is guaranteed to exist: the transaction
    //    above committed both card and terminal as one unit.
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

    // 5. Spawn the daemon. On failure we deliberately do NOT roll back the
    //    persisted rows — the orphan-terminal sweeper handles cleanup within
    //    its grace window. This matches the prior `routes/terminal.rs`
    //    semantics: a 500 here tells the client the spawn failed, but the
    //    card/terminal pair is still in the DB until the sweeper runs.
    spawn_terminal_for(&s, &term, &program, &cwd, &env).await?;

    Ok((StatusCode::CREATED, Json(card)))
}

/// Local copy of the default-shell resolver. The original lives in
/// `routes/terminal.rs` — we duplicate the one-liner here instead of
/// re-exporting so the two endpoints stay independently testable and the
/// shared module surface stays minimal.
fn default_program() -> String {
    let s = std::env::var("SHELL").unwrap_or_default();
    if s.is_empty() {
        "/bin/sh".to_string()
    } else {
        s
    }
}

/// Local copy of the default-cwd resolver — see `default_program`.
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
