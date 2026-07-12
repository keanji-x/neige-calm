use sqlx::Sqlite;
use sqlx::Transaction;

use super::{
    card_create_with_id_tx, card_delete_tx, card_update_tx, session_mcp_token_set_tx,
    session_projection_active_for_card_tx, session_start_runtime_tx,
    session_supersede_and_start_tx, terminal_create_tx, terminal_delete_tx,
};
use crate::card_kind::validate_card_kind_global;
use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::ids::WaveId;
use crate::model::*;
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionKind};
use crate::validation::{
    CLAUDE_PAYLOAD_SCHEMA_VERSION, CODEX_PAYLOAD_SCHEMA_VERSION, TERMINAL_PAYLOAD_SCHEMA_VERSION,
};
use calm_types::worker::WorkerSessionState;

/// Atomically create a `terminal`-kind card AND its associated terminal row
/// inside a single transaction. Runtime identity is written to
/// `worker_sessions`; API/WS responses project the legacy payload fields at
/// read time.
///
/// This is the kernel side of #13's plan to collapse today's 3-step
/// terminal-card recipe (card-add → terminal-create → card-update) into one
/// atomic db helper. PR1 just lands this helper; PR2 will wire it to a new
/// `POST /api/waves/:id/terminal-cards` endpoint and delete the old recipe.
///
/// On any failure the surrounding transaction rolls back, so partial state
/// (card without terminal, or terminal without worker-session row) is
/// impossible.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    wave_id: WaveId,
    title: Option<String>,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    role: CardRole,
    // Issue #229 PR A — required deletable bit, threaded through to
    // `card_create_with_id_tx`. Dispatcher's worker-terminal path passes
    // `true` (workers are user-facing — users can close them); the
    // direct `POST /api/waves/:id/terminal-cards` path passes `true` for
    // the same reason. Future kernel-owned terminal cards (none today)
    // would pass `false`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB, written onto the terminal row
    // alongside the card so every spawn path reads it from the row and
    // stamps consistent `--terminal-fg/-bg` argv (closes the WS auto-
    // revive race observed in PR #193).
    theme: RequestTheme,
) -> Result<(Card, Terminal)> {
    // 1. Card row with placeholder payload — schemaVersion is stamped in
    //    step 5 once we have the terminal row.
    //
    // PR2 of #136: card id is now pre-minted by the caller (same pattern
    // the codex helper has had since #117) so the surrounding
    // `write_with_event` can stamp `EventScope::Card { card, .. }` on
    // the audit row without racing the txn.
    //
    // User-facing terminal creation and dispatcher worker-terminal paths
    // pass `CardRole::Worker`. The cache
    // write-through inside `card_create_with_id_tx` keeps the role
    // visible to `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "terminal".into(),
            sort,
            payload: serde_json::Value::Null,
            title,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card.
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd,
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical terminal-card payload.
    let payload = serde_json::json!({
        "schemaVersion": TERMINAL_PAYLOAD_SCHEMA_VERSION,
    });

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs:141` already enforces this for direct create, but
    //    composing inside the kernel means we run our own check rather than
    //    trusting a payload we built ourselves.
    validate_card_kind_global("terminal", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    let runtime_init = WorkerSessionInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: WorkerSessionKind::Terminal,
        agent_provider: None,
        status: WorkerSessionState::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: spawn_op_id.map(str::to_string),
        now_ms: now_ms(),
    };
    if let Some(existing) = session_projection_active_for_card_tx(tx, card.id.as_ref()).await? {
        session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
    } else {
        session_start_runtime_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// Issue #310 followup — atomically delete a card + its backing terminal
/// row inside a single tx, in the order the `RESTRICT` FK demands
/// (terminal first, then card). The structural inverse of
/// [`card_with_terminal_create_tx`] / [`card_with_codex_create_tx`].
///
/// **Use site** is the dispatcher's post-commit failure cleanup: when
/// `per-card CODEX_HOME seeding` or `spawn_daemon_with_parts` returns
/// Err *after* the row-creation tx has already committed, the worker
/// card + terminal row are orphans — the runtime references a terminal
/// whose daemon never came up, and a retry with the same
/// `idempotency_key` would short-circuit on the abandoned row instead
/// of trying again. Rolling both rows back here lets the retry succeed.
///
/// **Idempotent shape.** Each delete swallows `NotFound` so a caller
/// that races the orphan sweeper (which deletes terminals out from
/// under us on a 30-60s cadence) still completes cleanly. The card
/// delete may still surface `NotFound` if the sweeper additionally
/// reaped the card — same shape as the route handler in
/// `routes/cards.rs::delete_card`, where the comment notes the same
/// race is acceptable.
///
/// `card_role_cache` is threaded through so the cache stays in
/// lockstep with the row delete — same write-through invariant
/// `card_delete_tx` itself enforces.
pub async fn card_with_terminal_rollback_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    terminal_id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    // Order matters — the FK on `terminals.card_id` is `ON DELETE RESTRICT`
    // since migration 0011, so the card delete would fail with a FK
    // violation if the terminal row still existed.
    match terminal_delete_tx(tx, terminal_id).await {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    match card_delete_tx(tx, card_id, card_role_cache).await {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Atomically create a `codex`-kind card, its associated terminal row, and
/// the initial `Starting` worker-session row inside a single transaction.
/// Runtime identity is written to `worker_sessions`; API/WS responses project
/// the legacy payload fields at read time.
///
/// Twin of [`card_with_terminal_create_tx`] for the codex-card flow (#117).
/// Differs in two places from the terminal helper:
///
///   1. The caller pre-mints `card_id` (option C in the design doc) so the
///      handler can derive per-card filesystem paths (`CODEX_HOME =
///      <codex_homes_dir>/<card_id>/`) before the row hits the DB. The
///      pre-mint avoids a post-commit "stamp env" round-trip that option B
///      would have required, and keeps a single `card.added` envelope on
///      the bus.
///   2. The canonical payload carries `cwd` when non-empty — the frontend's
///      `codex.tsx` placeholder reads it for status text while the daemon
///      boots. Terminal cards have no such field.
///
/// `program` is hardwired to `"codex"`. The caller still owns env
/// composition (CODEX_HOME / NEIGE_CARD_ID / proxy vars) since those
/// require `AppState` and a settings snapshot that the db layer shouldn't
/// see.
///
/// On any failure the surrounding transaction rolls back; a partial state
/// (card without terminal, or terminal without worker-session row) is
/// impossible.
/// PR7a (#136) — third return slot is `Some(raw_token)` for Spec/Worker
/// cards. The caller is expected to thread the raw value into the codex
/// daemon's `NEIGE_MCP_TOKEN` env var immediately and discard it — the
/// hash is persisted in `card_mcp_tokens`, but the raw form is
/// unrecoverable on a kernel restart (by design).
#[allow(clippy::too_many_arguments)]
pub async fn card_with_codex_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    wave_id: WaveId,
    title: Option<String>,
    sort: Option<f64>,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    role: CardRole,
    // Issue #229 PR A — required deletable bit. The wave-create route
    // passes `false` (the spec card is kernel-owned, must survive
    // direct REST / plugin-callback delete attempts). The user-facing
    // `POST /api/waves/:id/codex-cards` route passes `true`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB; written onto the terminal row
    // in the same transaction so the codex daemon's spawn argv is
    // deterministic regardless of which spawn path lands it.
    theme: RequestTheme,
) -> Result<(Card, Terminal, Option<String>)> {
    // 1. Card row with placeholder payload — schemaVersion and UI hints
    //    are stamped in step 5 once we have the terminal row.
    //
    // User-facing codex creation and dispatcher paths pass
    // `CardRole::Worker`. The wave-create route passes `CardRole::Spec`
    // so the auto-minted spec card is recognized by `enforce_role` as a
    // `WaveUpdated`-permitted emitter. The cache write-through
    // inside `card_create_with_id_tx` keeps the role visible to
    // `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "codex".into(),
            sort,
            payload: serde_json::Value::Null,
            title,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card. `program == "codex"` always —
    //    the codex CLI runs in the PTY directly (see `routes::codex_cards`).
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program: "codex".into(),
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical codex-card payload. `cwd` is omitted when the
    //    caller passed an empty string — the frontend treats a missing
    //    `cwd` as "show no path hint" rather than "show an empty path".
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    // `prompt` — surfaces to the `legacy auto-submit` subscriber, which
    // gates auto-Enter on this being a non-empty string. An empty /
    // missing value here is the "user spawned codex without a hands-free
    // prompt" path, identical to pre-#110 behaviour. Trimmed and empty-
    // filtered so the subscriber's `.filter(|s| !s.is_empty())` is the
    // single source of truth.
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs` enforces this for direct create; composing
    //    inside the kernel means we re-run the check on the payload we
    //    just built.
    validate_card_kind_global("codex", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    // 6. PR7a (#136) — when the card is Spec/Worker, mint a fresh per-card
    //    MCP token, store the hash in `card_mcp_tokens` inside the same tx
    //    (FK enforced — the card row above is the parent), and return the
    //    raw value to the caller so it can be threaded into the codex
    //    daemon's `NEIGE_MCP_TOKEN` env var.
    //
    //    Doing this here (rather than at the route layer) keeps the
    //    invariant atomic: a committed card row whose role is Spec/Worker
    //    will *always* have a matching token row, and a rolled-back tx
    //    drops both together.
    let mut mcp_token_hash = None;
    let mcp_token = if matches!(role, CardRole::Spec | CardRole::Worker) {
        let token = crate::mcp_auth::CardMcpToken::generate();
        let hashed = crate::mcp_auth::hash_token(token.as_str());
        card_mcp_token_set_tx(tx, card.id.as_ref(), &hashed).await?;
        mcp_token_hash = Some(hashed);
        Some(token.into_inner())
    } else {
        None
    };

    let runtime_init = WorkerSessionInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: WorkerSessionKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: spawn_op_id.map(str::to_string),
        now_ms: now_ms(),
    };
    session_start_runtime_tx(tx, runtime_init).await?;
    if let Some(hashed) = mcp_token_hash.as_deref() {
        session_mcp_token_set_tx(tx, runtime_id, hashed).await?;
    }

    Ok((card, term, mcp_token))
}

/// Atomically create a `claude`-kind worker card AND its associated terminal
/// row. Claude cards are PTY-backed like codex cards, but intentionally have
/// no MCP token/config path; completion observability comes solely from
/// Claude hook events ingested through `/internal/claude/hook`.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    wave_id: WaveId,
    title: Option<String>,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    settings_path: String,
    claude_session_id: String,
    role: CardRole,
    deletable: bool,
    card_role_cache: &CardRoleCache,
    theme: RequestTheme,
) -> Result<(Card, Terminal)> {
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "claude".into(),
            sort,
            payload: serde_json::Value::Null,
            title,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CLAUDE_PAYLOAD_SCHEMA_VERSION),
    );
    payload.insert(
        "settings_path".into(),
        serde_json::Value::String(settings_path),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);
    validate_card_kind_global("claude", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;

    let runtime_init = WorkerSessionInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: WorkerSessionKind::ClaudeCard,
        agent_provider: Some(AgentProvider::Claude),
        status: WorkerSessionState::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: Some(claude_session_id),
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    };
    if let Some(existing) = session_projection_active_for_card_tx(tx, card.id.as_ref()).await? {
        session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
    } else {
        session_start_runtime_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// Atomically create a scheduler-owned `claude` worker card and terminal.
///
/// This mirrors [`card_with_claude_create_tx`] for the persisted card shape,
/// but is specific to first-class task workers: the role is always
/// [`CardRole::Worker`], `spawn_op_id` is recorded for reaper convergence,
/// and the worker session row is seeded without minting a raw MCP token.
/// The spawn-side effect rotates the token post-commit and writes only the
/// hash back into `card_mcp_tokens` and `worker_sessions`.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_worker_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    wave_id: WaveId,
    title: Option<String>,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    settings_path: String,
    claude_session_id: String,
    card_role_cache: &CardRoleCache,
    theme: RequestTheme,
) -> Result<(Card, Terminal)> {
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "claude".into(),
            sort,
            payload: serde_json::Value::Null,
            title,
        },
        CardRole::Worker,
        true,
        card_role_cache,
    )
    .await?;

    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CLAUDE_PAYLOAD_SCHEMA_VERSION),
    );
    payload.insert(
        "settings_path".into(),
        serde_json::Value::String(settings_path),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);
    validate_card_kind_global("claude", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;

    let runtime_init = WorkerSessionInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: WorkerSessionKind::ClaudeCard,
        agent_provider: Some(AgentProvider::Claude),
        status: WorkerSessionState::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: Some(claude_session_id),
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: spawn_op_id.map(str::to_string),
        now_ms: now_ms(),
    };
    session_start_runtime_tx(tx, runtime_init).await?;

    Ok((card, term))
}

/// PR7a (#136) — insert (or replace) a per-card MCP token row in the
/// supplied transaction. The raw token is never persisted; the caller
/// passes `hash_token(raw)` and keeps the raw value only in memory long
/// enough to thread it into the env map handed to the codex daemon.
///
/// `card_id` must reference a real row in `cards` — the FK constraint
/// in migration 0010 fails the tx otherwise. The standard call site is
/// `card_with_codex_create_tx`, where the card row is created moments
/// earlier in the same tx, so the FK is satisfied by construction.
pub async fn card_mcp_token_set_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    hashed_token: &str,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(card_id) DO UPDATE SET
               hashed_token = excluded.hashed_token,
               created_at   = excluded.created_at"#,
    )
    .bind(card_id)
    .bind(hashed_token)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::db::{RepoRead, RepoSyncDomainRaw};
    use serde_json::json;

    #[tokio::test]
    async fn card_title_round_trips_through_create_patch_and_composite() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: "title-test".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id,
                title: "wave".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();

        let card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                title: Some("Hello".into()),
                kind: "plugin:test:view".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .unwrap();
        assert_eq!(
            repo.card_get(card.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Hello")
        );
        assert_eq!(
            repo.cards_by_wave(wave.id.as_str()).await.unwrap()[0]
                .title
                .as_deref(),
            Some("Hello")
        );

        repo.card_update(
            card.id.as_str(),
            CardPatch {
                title: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            repo.card_get(card.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Renamed")
        );

        let mut tx = repo.pool().begin().await.unwrap();
        let (composite, _) = card_with_terminal_create_tx(
            &mut tx,
            crate::model::new_id(),
            &crate::model::new_id(),
            None,
            wave.id,
            Some("T".into()),
            None,
            "bash".into(),
            "/tmp".into(),
            json!({}),
            CardRole::Worker,
            true,
            repo.card_role_cache(),
            RequestTheme::default_dark(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            repo.card_get(composite.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("T")
        );
    }
}
