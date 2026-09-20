use sqlx::Sqlite;
use sqlx::Transaction;

use super::{
    card_create_with_id_tx, card_delete_tx, card_update_tx, session_mcp_token_set_tx,
    session_projection_active_for_card_tx, session_start_runtime_tx,
    session_supersede_and_start_tx, terminal_create_tx, terminal_delete_tx,
};
use crate::card_kind::validate_card_kind_global;
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::model::*;
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionKind};
use crate::validation::{
    CLAUDE_PAYLOAD_SCHEMA_VERSION, CODEX_PAYLOAD_SCHEMA_VERSION,
    TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY, TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY,
    TERMINAL_PAYLOAD_SCHEMA_VERSION, TERMINAL_SIGNALS_PAYLOAD_KEY,
};
use calm_types::claude_permissions::ClaudePermissionsSource;
use calm_types::worker::WorkerSessionState;

/// Atomically create a `terminal`-kind card AND its terminal row in a single transaction; runtime identity is written
/// to `worker_sessions`, and legacy payload fields are projected at read time. On any failure the whole tx rolls back.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    track_id: TrackId,
    title: Option<String>,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    role: CardRole,
    // Required deletable bit; worker terminals are user-facing and pass `true`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // Host browser's theme RGB, written onto the terminal row so every spawn path stamps consistent `--terminal-fg/-bg` argv.
    theme: RequestTheme,
    // `true` only for a terminal the Planner opened with hook signals: stamps `TERMINAL_SIGNALS_PAYLOAD_KEY` into the
    // card payload, the durable provenance the hook ingest route keys on.
    planner_hooks: bool,
) -> Result<(Card, Terminal)> {
    // Card row with placeholder payload; schemaVersion is stamped once the terminal row exists. The card id is
    // pre-minted by the caller so `write_with_event` can stamp `EventScope::Card` without racing the txn.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            track_id,
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

    let mut payload = serde_json::json!({
        "schemaVersion": TERMINAL_PAYLOAD_SCHEMA_VERSION,
    });
    if planner_hooks {
        payload[TERMINAL_SIGNALS_PAYLOAD_KEY] = serde_json::Value::Bool(true);
    }

    // Defense-in-depth: re-run payload validation on the payload we built ourselves.
    validate_card_kind_global("terminal", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            // Kernel-internal callers never patch `deletable`.
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

/// Stamp the effective Claude Code `permissions` block a Planner declared on `calm.terminal.open` into the card payload,
/// with its source beside it, inside the caller's transaction. Only the kernel ever calls this: every public write boundary refuses the keys.
pub async fn card_stamp_claude_permissions_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &Card,
    block: serde_json::Value,
    source: ClaudePermissionsSource,
) -> Result<Card> {
    let mut payload = card.payload.clone();
    let Some(map) = payload.as_object_mut() else {
        return Err(CalmError::Internal(format!(
            "card {} payload is not a JSON object; cannot stamp `{TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY}`",
            card.id
        )));
    };
    map.insert(TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY.to_owned(), block);
    map.insert(
        TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY.to_owned(),
        serde_json::Value::String(source.as_str().to_owned()),
    );
    validate_card_kind_global(&card.kind, &payload)?;
    card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            payload: Some(payload),
            ..Default::default()
        },
    )
    .await
}

/// Atomically delete a card + its backing terminal row (terminal first, as the `RESTRICT` FK demands). Used for the
/// dispatcher's post-commit spawn-failure cleanup, so a retry with the same idempotency key does not short-circuit on
/// the orphan. Each delete swallows `NotFound` because the orphan sweeper may race it.
pub async fn card_with_terminal_rollback_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    terminal_id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    // Order matters — the FK on `terminals.card_id` is `ON DELETE RESTRICT`.
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

/// Atomically create a `codex`-kind card, its terminal row, and the initial `Starting` worker-session row. The caller
/// pre-mints `card_id` so per-card filesystem paths (`CODEX_HOME`) can be derived before the row exists. The third
/// return slot is the raw MCP token for Planner/Worker cards: thread it into `NEIGE_MCP_TOKEN` immediately and discard it — only the hash is persisted.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_codex_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    track_id: TrackId,
    title: Option<String>,
    sort: Option<f64>,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    role: CardRole,
    // Required deletable bit: the planner card is kernel-owned and passes `false`; user-facing codex cards pass `true`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // Host browser's theme RGB, written onto the terminal row in the same transaction so the spawn argv is deterministic.
    theme: RequestTheme,
) -> Result<(Card, Terminal, Option<String>)> {
    // Card row with placeholder payload; the track-create route passes `CardRole::Planner` so the auto-minted planner
    // card is recognized by `enforce_role` as a `TrackUpdated`-permitted emitter.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            track_id,
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

    // `program == "codex"` always — the codex CLI runs in the PTY directly.
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

    // `cwd` is omitted when empty: the frontend treats a missing `cwd` as "no path hint", not an empty path.
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    // `prompt` gates the auto-submit subscriber; trimmed and empty-filtered so the subscriber's filter is the single source of truth.
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

    // Defense-in-depth: re-run payload validation on the payload we built ourselves.
    validate_card_kind_global("codex", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            // Kernel-internal callers never patch `deletable`.
            deletable: None,
        },
    )
    .await?;

    // For Planner/Worker cards, mint a per-card MCP token and store its hash in the same tx, so a committed card row
    // with that role *always* has a matching token row.
    let mut mcp_token_hash = None;
    let mcp_token = if matches!(role, CardRole::Planner | CardRole::Worker) {
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

/// Atomically create a `claude`-kind worker card AND its terminal row. Claude cards intentionally have no MCP
/// token/config path; completion observability comes solely from Claude hook events.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    track_id: TrackId,
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
            track_id,
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

/// Atomically create a scheduler-owned `claude` worker card and terminal: role is always `Worker`, `spawn_op_id` is
/// recorded for reaper convergence, and the session row is seeded without a raw MCP token (rotated post-commit).
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_worker_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    track_id: TrackId,
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
            track_id,
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

/// Insert (or replace) a per-card MCP token row; the raw token is never persisted, the caller passes `hash_token(raw)`.
/// `card_id` must reference a real `cards` row (FK).
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
    use crate::error::CalmError;
    use serde_json::json;

    #[tokio::test]
    async fn card_title_round_trips_through_create_patch_and_composite() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "title-test".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();

        let card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
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
            repo.cards_by_track(track.id.as_str()).await.unwrap()[0]
                .title
                .as_deref(),
            Some("Hello")
        );

        let untitled = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: Some(String::new()),
                kind: "plugin:test:view".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .unwrap();
        assert_eq!(
            repo.card_get(untitled.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .title,
            None
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

        repo.card_update(card.id.as_str(), CardPatch::default())
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

        for title in ["", "  "] {
            repo.card_update(
                card.id.as_str(),
                CardPatch {
                    title: Some(title.into()),
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
                    .title,
                None
            );
            repo.card_update(
                card.id.as_str(),
                CardPatch {
                    title: Some("Restored".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        let mut tx = repo.pool().begin().await.unwrap();
        let (composite, _) = card_with_terminal_create_tx(
            &mut tx,
            crate::model::new_id(),
            &crate::model::new_id(),
            None,
            track.id,
            Some("T".into()),
            None,
            "bash".into(),
            "/tmp".into(),
            json!({}),
            CardRole::Worker,
            true,
            repo.card_role_cache(),
            RequestTheme::default_dark(),
            false,
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

    /// `card_update_tx` keeps a stored `terminal_signals: true` sticky across a whole-payload replacement, refuses a
    /// replacement that cannot carry it, and never mints it on a card that lacks it.
    #[tokio::test]
    async fn card_update_keeps_the_planner_terminal_marker_sticky() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "marker".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let open = |planner_hooks: bool| {
            let track_id = track.id.clone();
            let repo = &repo;
            async move {
                let mut tx = repo.pool().begin().await.unwrap();
                let (card, _) = card_with_terminal_create_tx(
                    &mut tx,
                    crate::model::new_id(),
                    &crate::model::new_id(),
                    None,
                    track_id,
                    None,
                    None,
                    "bash".into(),
                    "/tmp".into(),
                    json!({}),
                    CardRole::Worker,
                    true,
                    repo.card_role_cache(),
                    RequestTheme::default_dark(),
                    planner_hooks,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                card
            }
        };
        let marked = open(true).await;
        let plain = open(false).await;
        assert_eq!(marked.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);
        assert!(plain.payload.get(TERMINAL_SIGNALS_PAYLOAD_KEY).is_none());

        let replaced = repo
            .card_update(
                marked.id.as_str(),
                CardPatch {
                    payload: Some(json!({ "schemaVersion": 1, "terminal_id": "x" })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(replaced.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);
        assert_eq!(replaced.payload["terminal_id"], "x");
        assert_eq!(
            repo.card_get(marked.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .payload[TERMINAL_SIGNALS_PAYLOAD_KEY],
            true
        );
        // Sticky across a kind retarget as well (the hook route keys on the payload, never on the patchable kind).
        let retargeted = repo
            .card_update(
                marked.id.as_str(),
                CardPatch {
                    kind: Some("codex".into()),
                    payload: Some(json!({ "schemaVersion": 1 })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(retargeted.kind, "codex");
        assert_eq!(retargeted.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);

        let err = repo
            .card_update(
                marked.id.as_str(),
                CardPatch {
                    payload: Some(json!("not an object")),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CalmError::Core(calm_types::error::CoreError::BadRequest(_))
            ),
            "{err:?}"
        );
        assert_eq!(
            repo.card_get(marked.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .payload[TERMINAL_SIGNALS_PAYLOAD_KEY],
            true,
            "the refused replacement wrote nothing"
        );

        let plain = repo
            .card_update(
                plain.id.as_str(),
                CardPatch {
                    payload: Some(json!({ "schemaVersion": 1, "terminal_id": "y" })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            plain.payload.get(TERMINAL_SIGNALS_PAYLOAD_KEY).is_none(),
            "an update never mints the marker: {}",
            plain.payload
        );
    }

    /// `card_update_tx` keeps BOTH server-owned keys sticky across a whole-payload replacement.
    #[tokio::test]
    async fn card_update_keeps_claude_permissions_sticky() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "permissions".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let block = json!({
            "allow": ["Edit(//w/**)", "Bash(git status *)"],
            "ask": ["Bash(git push *)", "Edit(//w/.git/**)"],
            "deny": ["Bash(git push *)"],
        });
        let open = |stamp: bool| {
            let track_id = track.id.clone();
            let repo = &repo;
            let block = block.clone();
            async move {
                let mut tx = repo.pool().begin().await.unwrap();
                let (card, _) = card_with_terminal_create_tx(
                    &mut tx,
                    crate::model::new_id(),
                    &crate::model::new_id(),
                    None,
                    track_id,
                    None,
                    None,
                    "bash".into(),
                    "/w".into(),
                    json!({}),
                    CardRole::Worker,
                    true,
                    repo.card_role_cache(),
                    RequestTheme::default_dark(),
                    true,
                )
                .await
                .unwrap();
                let card = if stamp {
                    card_stamp_claude_permissions_tx(
                        &mut tx,
                        &card,
                        block,
                        ClaudePermissionsSource::DeclaredWithinPolicy,
                    )
                    .await
                    .unwrap()
                } else {
                    card
                };
                tx.commit().await.unwrap();
                card
            }
        };
        let stamped = open(true).await;
        let plain = open(false).await;
        assert_eq!(
            stamped.payload[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY],
            block
        );
        // The source is stamped beside the block, in the wire spelling.
        assert_eq!(
            stamped.payload[TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY],
            "declared_within_policy"
        );
        assert_eq!(stamped.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);
        assert_eq!(stamped.payload["schemaVersion"], 1);
        assert_eq!(
            repo.card_get(stamped.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .payload,
            stamped.payload,
            "the returned card is the stored one"
        );
        assert!(
            plain
                .payload
                .get(TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY)
                .is_none()
        );

        let replaced = repo
            .card_update(
                stamped.id.as_str(),
                CardPatch {
                    payload: Some(json!({ "schemaVersion": 1, "terminal_id": "x" })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            replaced.payload[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY],
            block
        );
        assert_eq!(
            replaced.payload[TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY],
            "declared_within_policy",
            "the source is sticky across a replacement"
        );
        assert_eq!(replaced.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);
        assert_eq!(replaced.payload["terminal_id"], "x");
        assert_eq!(
            repo.card_get(stamped.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .payload[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY],
            block
        );

        let err = repo
            .card_update(
                stamped.id.as_str(),
                CardPatch {
                    payload: Some(json!([1])),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CalmError::Core(calm_types::error::CoreError::BadRequest(_))
            ),
            "{err:?}"
        );
        assert_eq!(
            repo.card_get(stamped.id.as_str())
                .await
                .unwrap()
                .unwrap()
                .payload[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY],
            block,
            "the refused replacement wrote nothing"
        );

        let plain = repo
            .card_update(
                plain.id.as_str(),
                CardPatch {
                    payload: Some(json!({ "schemaVersion": 1, "terminal_id": "y" })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            plain
                .payload
                .get(TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY)
                .is_none(),
            "an update never mints the block: {}",
            plain.payload
        );
        assert!(
            plain
                .payload
                .get(TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY)
                .is_none(),
            "an update never mints the source: {}",
            plain.payload
        );
        assert_eq!(plain.payload[TERMINAL_SIGNALS_PAYLOAD_KEY], true);

        // Only the kernel-minted shapes are sticky: a stored `false`, `null`, or unknown source spelling is NOT re-inserted,
        // and such a card accepts a non-object replacement.
        let odd = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "terminal".into(),
                sort: None,
                payload: json!({
                    "schemaVersion": 1,
                    TERMINAL_SIGNALS_PAYLOAD_KEY: false,
                    TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY: null,
                    TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY: "policy",
                }),
            })
            .await
            .unwrap();
        let replaced = repo
            .card_update(
                odd.id.as_str(),
                CardPatch {
                    payload: Some(json!({ "schemaVersion": 1, "terminal_id": "z" })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            replaced.payload,
            json!({ "schemaVersion": 1, "terminal_id": "z" }),
            "a false marker, a null block and an unknown source are not sticky"
        );
        let odd = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "terminal".into(),
                sort: None,
                payload: json!({ "schemaVersion": 1, TERMINAL_SIGNALS_PAYLOAD_KEY: false }),
            })
            .await
            .unwrap();
        let replaced = repo
            .card_update(
                odd.id.as_str(),
                CardPatch {
                    payload: Some(json!("not an object")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            replaced.payload,
            json!("not an object"),
            "without a sticky value a non-object replacement is accepted"
        );
    }
}
