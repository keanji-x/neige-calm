//! Calm kernel — minimal container/PTY core. Business semantics (tasks,
//! calendar, plans, git, ...) live in out-of-process plugins reached via MCP.
//!
//! Module map:
//! ```text
//! model         entity types + DTOs (Cove/Wave/Card/Overlay/Terminal/Plugin)
//! error         CalmError + Result alias + IntoResponse
//! event         Event enum + EventBus (broadcast fan-out)
//! db            Repo trait
//!   ├ mod.rs    `Repo` trait + helper free fns
//!   └ sqlite.rs SqlxRepo (production + in-memory dev/test default via
//!               `sqlite::memory:`)
//! routes        HTTP API
//!   ├ coves.rs       (track B)
//!   ├ waves.rs       (track B)
//!   ├ cards.rs       (track B)
//!   ├ overlays.rs    (track B)
//!   ├ plugins.rs     (M2 stub)
//!   └ terminal.rs    (track D, REST half)
//! ws            WebSocket endpoints
//!   ├ events.rs      (track C)
//!   └ terminal.rs    (track D, WS half)
//! plugin_host   M2 placeholder
//! state         AppState (Arc<Repo>, EventBus, DaemonClient, PluginHost)
//! config        Config (CLI / env)
//! ```

pub mod actor;
/// #322 — aspect / join-point framework: OCP-shaped invariant enforcement.
/// See [`aspect`] module docs for the closed-set / open-impl split. Lives at
/// the module-list head so reviewers see the framework boundary up top.
pub mod aspect;
pub mod auth;

/// #388 Phase 3b — reconcile DB rows that still look live with the
/// process supervisor's PTY registry. Production no longer respawns
/// daemon binaries at boot. If the supervisor does not know a supposedly
/// running terminal, mark the row exited with the stale-row sentinel `-1`
/// and move on.
pub async fn reconcile_supervisor_on_boot(state: &state::AppState) {
    let rows = match state.repo.terminals_running().await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reconcile_supervisor_on_boot: list-running query failed; skipping sweep"
            );
            return;
        }
    };
    let mut running = 0usize;
    let mut stale = 0usize;
    for term in rows {
        match probe_supervisor_for_terminal(state, &term.id).await {
            Ok(true) => running += 1,
            Ok(false) => {
                stale += 1;
                tracing::warn!(
                    terminal_id = %term.id,
                    "terminal row is running in DB but supervisor has no live PTY; marking exited",
                );
                if let Err(e) = state
                    .repo
                    .terminal_set_exit(&term.id, Some(-1), false)
                    .await
                {
                    tracing::warn!(
                        terminal_id = %term.id,
                        error = %e,
                        "failed to mark stale terminal exited during boot reconcile"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    terminal_id = %term.id,
                    error = %e,
                    "supervisor probe failed during boot reconcile; leaving row unchanged"
                );
            }
        }
    }
    tracing::info!(running, stale, "reconcile_supervisor_on_boot: complete",);
}

pub async fn revive_orphans_on_boot(state: &state::AppState) {
    reconcile_supervisor_on_boot(state).await;
}

pub(crate) async fn probe_supervisor_for_terminal(
    state: &state::AppState,
    terminal_id: &str,
) -> anyhow::Result<bool> {
    use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
    use calm_session::{read_frame, write_frame};
    use tokio::net::UnixStream;

    let sock =
        crate::proc_supervisor::resolve_control_sock(state.daemon.proc_supervisor_sock.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut stream = UnixStream::connect(&sock)
        .await
        .map_err(|e| anyhow::anyhow!("connect proc supervisor {}: {e}", sock.display()))?;
    write_frame(
        &mut stream,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: format!("term:{terminal_id}"),
        }),
    )
    .await?;
    match read_frame(&mut stream).await? {
        ControlReply::ProbeOk { proc_running, .. } => Ok(proc_running),
        ControlReply::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected supervisor probe reply: {other:?}"),
    }
}

#[allow(dead_code)]
fn boot_revive_program_for_terminal(
    term: &model::Terminal,
    card: Option<&model::Card>,
    claude_bin: &str,
) -> String {
    let Some(card) = card else {
        return term.program.clone();
    };
    if card.kind != "claude" {
        return term.program.clone();
    }
    let payload = &card.payload;
    let Some(claude_session_id) = payload
        .get("claude_session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return term.program.clone();
    };
    let Some(settings_path) = payload
        .get("settings_path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return term.program.clone();
    };

    format!(
        "{} --settings {} --resume {}",
        routes::codex_cards::shell_single_quote(claude_bin),
        routes::codex_cards::shell_single_quote(settings_path),
        routes::codex_cards::shell_single_quote(claude_session_id),
    )
}

#[cfg(test)]
mod claude_boot_revive_tests {
    use super::*;
    use serde_json::json;

    fn terminal(program: &str, cwd: &str) -> model::Terminal {
        model::Terminal {
            id: "term-1".into(),
            card_id: "card-1".into(),
            program: program.into(),
            cwd: cwd.into(),
            env: json!({"NEIGE_HOOK_PROVIDER": "claude"}),
            pid: None,
            theme_fg: "216,219,226".into(),
            theme_bg: "15,20,24".into(),
            exit_code: None,
            signal_killed: false,
            created_at: 0,
        }
    }

    fn card(kind: &str, payload: serde_json::Value) -> model::Card {
        model::Card {
            id: "card-1".into(),
            wave_id: "wave-1".into(),
            kind: kind.into(),
            sort: 0.0,
            payload,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn claude_boot_revive_rebuilds_resume_command_from_payload() {
        let term = terminal(
            "'/opt/claude' --settings '/tmp/settings.json' --session-id '11111111-1111-4111-8111-111111111111' -- 'first prompt'",
            "/workspace",
        );
        let claude = card(
            "claude",
            json!({
                "schemaVersion": 1,
                "terminal_id": "term-1",
                "settings_path": "/tmp/settings.json",
                "cwd": "/workspace",
                "prompt": "first prompt",
                "claude_session_id": "22222222-2222-4222-8222-222222222222"
            }),
        );

        let program = boot_revive_program_for_terminal(&term, Some(&claude), "/opt/claude");

        assert_eq!(
            program,
            "'/opt/claude' --settings '/tmp/settings.json' --resume '22222222-2222-4222-8222-222222222222'"
        );
        assert!(!program.contains("--session-id"));
        assert!(!program.contains("--fork-session"));
        assert!(!program.contains("first prompt"));
        assert_eq!(term.cwd, "/workspace");
    }

    #[test]
    fn claude_boot_revive_without_session_id_keeps_legacy_fresh_spawn_program() {
        let original = "'/opt/claude' --settings '/tmp/settings.json' -- 'first prompt'";
        let term = terminal(original, "/workspace");
        let legacy = card(
            "claude",
            json!({
                "schemaVersion": 1,
                "terminal_id": "term-1",
                "settings_path": "/tmp/settings.json",
                "cwd": "/workspace",
                "prompt": "first prompt"
            }),
        );

        let program = boot_revive_program_for_terminal(&term, Some(&legacy), "/opt/claude");

        assert_eq!(program, original);
        assert!(!program.contains("--resume"));
    }
}

/// #313 problem #1 — boot-time **takeover** of in-flight spec waves.
///
/// Replaces the previous "boot-time kill" sweep
/// (`reap_orphan_appserver_groups_on_boot` — see the parent design in
/// issue #313). Today's posture is: every spec card whose payload carries
/// a `codex_thread_id` AND whose wave is **not terminal** gets:
///
///   1. A live `codex app-server` re-established for it — reused if the
///      previous process is still bound to its persisted socket (kernel
///      hard-crash → systemd reparenting), else freshly respawned (graceful
///      teardown / `kill_on_drop` reaped it on the way down).
///   2. `initialize` + `thread/resume(<codex_thread_id>)` on that server —
///      based on the on-disk rollout. Empty-goal waves that intentionally
///      skipped the first turn carry `appserver_needs_initial_prompt` and
///      are excluded from this resume path; boot starts a fresh idle thread
///      for them instead.
///   3. A fresh [`SpecPushHandle`] registered in [`SpecPushRegistry`]
///      keyed by [`crate::ids::WaveId`], identical to what
///      [`crate::routes::waves::create_wave`] would have inserted.
///   4. **Catch-up replay** of every persisted event with `id >
///      push_watermark` for that wave, routed through the dispatcher's
///      normal push path ([`crate::dispatcher::Dispatcher::catch_up_push`])
///      so dedup, queue, and turn-phase semantics are byte-identical to
///      steady-state delivery. The in-memory
///      [`crate::event_cursor::EventCursorCache`] is seeded from the
///      persisted `push_watermark` BEFORE the replay starts.
///
/// Every failure mode is **non-fatal at boot** (boot stays best-effort,
/// matching `create_wave`'s 201-when-spec-fails posture):
///
///   * `thread/resume` returns `-32600 "no rollout found"` (a legacy or
///     malformed row is resumable in SQL but has no rollout on disk) →
///     log warn, clear the stale push fields (`codex_thread_id`, sock,
///     pgid, watermark) so the next boot doesn't retry, leave the wave
///     inert. Matches the "lazy wave" state from issue #313 problem #2
///     (out of scope for this PR — just don't crash).
///   * App-server fails to spawn or the socket never becomes ready → log
///     warn, leave the wave inert. The next boot will retry.
///   * The wave's lifecycle is terminal — SQL `WHERE` already filtered it
///     out; this path never sees it.
///   * `codex_thread_id` is absent — SQL `WHERE` filtered it out; this
///     path never sees it either.
///   * Any individual wave's takeover failing must NOT fail the kernel
///     boot.
///
/// Preserves the #293/#311 push-path invariants: no pull fallback, dedup
/// via `envelope_id > push_watermark`, mid-turn queue semantics on
/// resumed handles (the `SpecPushHandle` produced by resume goes through
/// the same `consume_notifications` task as one produced by spawn).
pub async fn takeover_spec_appservers_on_boot(state: &state::AppState) {
    // #328 P2 (non-Linux warn) — `spec_appserver::verify_owned_pid` is a
    // `/proc`-backed Linux-only identity check; on macOS / BSD the stub
    // returns `false` unconditionally and every kill on this path is
    // silently skipped. Production hosts are Linux, but a dev box on
    // macOS would never see the kill path exercise, and the silence
    // hides that. Emit a one-shot warn at boot so the operator at least
    // sees in the log that the reap is degraded to "rely on the
    // respawn's `bind(2)` to fail loudly if the old socket is still
    // bound".
    #[cfg(not(target_os = "linux"))]
    tracing::warn!(
        "takeover_spec_appservers_on_boot: non-linux target — \
         verify_owned_pid stub returns false; persisted app-server \
         process groups will NOT be reaped, falling back to bind(2) \
         conflict surfaced by the respawn"
    );
    let cards = match spec_cards_for_boot_takeover_table_first(state.repo.as_ref()).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "takeover_spec_appservers_on_boot: query failed; skipping"
            );
            return;
        }
    };
    let initial_prompt_cards = match state.repo.spec_cards_for_initial_prompt_bootstrap().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "takeover_spec_appservers_on_boot: initial-prompt query failed; skipping"
            );
            Vec::new()
        }
    };
    if cards.is_empty() && initial_prompt_cards.is_empty() {
        tracing::info!("takeover_spec_appservers_on_boot: no in-flight spec waves to take over");
        return;
    }
    tracing::info!(
        candidates = cards.len(),
        initial_prompt_candidates = initial_prompt_cards.len(),
        "takeover_spec_appservers_on_boot: starting boot takeover"
    );
    // Settings drive the proxy env handed to a respawned app-server (same
    // shape `create_wave` builds via `build_codex_env_map`).
    let settings = match crate::routes::settings::load_settings(state.repo.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            // A settings load failure shouldn't block takeover — fall back
            // to empty proxies (the app-server still boots, just without
            // an override). `create_wave` would surface this as a 500 on
            // the hot path, but at boot we prefer "best-effort proceed".
            tracing::warn!(
                error = %e,
                "takeover_spec_appservers_on_boot: load_settings failed; proceeding with no proxy override"
            );
            crate::routes::settings::Settings::default()
        }
    };

    let mut respawned = 0usize;
    let mut initial_prompt_respawned = 0usize;
    let mut inert = 0usize;
    for (
        card_id,
        wave_id,
        thread_id,
        persisted_pgid,
        persisted_sock,
        persisted_start_time,
        persisted_boot_id,
        watermark,
    ) in cards
    {
        let wave_key: crate::ids::WaveId = wave_id.clone().into();
        // Per-wave best-effort: failures inside this block are logged and
        // we move on to the next wave (the kernel boot proceeds regardless).
        let outcome = try_takeover_one_wave(
            state,
            &settings,
            &card_id,
            &wave_key,
            &thread_id,
            persisted_pgid,
            persisted_sock.as_deref(),
            persisted_start_time,
            persisted_boot_id.as_deref(),
            watermark,
        )
        .await;
        match outcome {
            TakeoverOutcome::Respawned => respawned += 1,
            TakeoverOutcome::Inert => inert += 1,
        }
    }
    for (
        card_id,
        wave_id,
        cwd,
        persisted_pgid,
        persisted_sock,
        persisted_start_time,
        persisted_boot_id,
        watermark,
    ) in initial_prompt_cards
    {
        let wave_key: crate::ids::WaveId = wave_id.clone().into();
        let outcome = bootstrap_empty_goal_spec_appserver(
            state,
            &settings,
            &card_id,
            &wave_key,
            &cwd,
            persisted_pgid,
            persisted_sock.as_deref(),
            persisted_start_time,
            persisted_boot_id.as_deref(),
            watermark,
        )
        .await;
        match outcome {
            TakeoverOutcome::Respawned => initial_prompt_respawned += 1,
            TakeoverOutcome::Inert => inert += 1,
        }
    }
    tracing::info!(
        respawned,
        initial_prompt_respawned,
        inert,
        "takeover_spec_appservers_on_boot: complete"
    );
}

type BootTakeoverSpecCard = (
    String,
    String,
    String,
    Option<i32>,
    Option<String>,
    Option<u64>,
    Option<String>,
    i64,
);

pub(crate) async fn spec_cards_for_boot_takeover_table_first(
    repo: &dyn crate::db::RepoRead,
) -> crate::error::Result<Vec<BootTakeoverSpecCard>> {
    let mappings = repo.card_codex_threads_active().await?;
    let mapped_card_ids: std::collections::HashSet<String> =
        mappings.iter().map(|row| row.card_id.clone()).collect();
    let mut rows = Vec::new();

    for mapping in mappings {
        if mapping.role != crate::model::CardRole::Spec {
            continue;
        }
        let Some(wave_id) = mapping.wave_id.clone() else {
            tracing::warn!(
                card_id = %mapping.card_id,
                thread_id = %mapping.thread_id,
                "takeover_spec_appservers_on_boot: spec thread mapping has no wave_id; skipping"
            );
            continue;
        };
        let Some(wave) = repo.wave_get(&wave_id).await? else {
            continue;
        };
        if matches!(
            wave.lifecycle,
            crate::model::WaveLifecycle::Done
                | crate::model::WaveLifecycle::Canceled
                | crate::model::WaveLifecycle::Failed
        ) {
            continue;
        }
        let Some(card) = repo.card_get(&mapping.card_id).await? else {
            continue;
        };
        if payload_needs_initial_prompt(&card.payload) {
            continue;
        }
        let (pgid, sock, start_time, boot_id, watermark) =
            boot_takeover_payload_fields(&card.payload);
        rows.push((
            mapping.card_id,
            wave_id,
            mapping.thread_id,
            pgid,
            sock,
            start_time,
            boot_id,
            watermark,
        ));
    }

    for legacy in repo.spec_cards_for_boot_takeover().await? {
        if !mapped_card_ids.contains(&legacy.0) {
            rows.push(legacy);
        }
    }

    Ok(rows)
}

fn payload_needs_initial_prompt(payload: &serde_json::Value) -> bool {
    payload
        .get("appserver_needs_initial_prompt")
        .is_some_and(|value| value.as_i64() == Some(1) || value.as_bool() == Some(true))
}

fn boot_takeover_payload_fields(
    payload: &serde_json::Value,
) -> (
    Option<i32>,
    Option<String>,
    Option<u64>,
    Option<String>,
    i64,
) {
    let pgid = payload
        .get("appserver_pgid")
        .and_then(serde_json::Value::as_i64)
        .and_then(|v| i32::try_from(v).ok());
    let sock = payload
        .get("appserver_sock")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let start_time = payload
        .get("appserver_start_time")
        .and_then(serde_json::Value::as_u64);
    let boot_id = payload
        .get("appserver_boot_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let watermark = payload
        .get("push_watermark")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    (pgid, sock, start_time, boot_id, watermark)
}

#[cfg(test)]
mod pr2_thread_mapping_tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{CardRole, NewCard, NewCove, NewWave};
    use serde_json::json;

    #[tokio::test]
    async fn boot_takeover_uses_table_before_payload() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: "c".into(),
                color: "#000000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();

        let cache = CardRoleCache::new();
        let mut tx = repo.pool().begin().await.unwrap();
        let card = crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            crate::model::new_id(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({
                    "codex_thread_id": "thread-from-payload",
                    "appserver_pgid": 1234,
                    "appserver_sock": "/tmp/spec.sock",
                    "push_watermark": 42,
                }),
            },
            CardRole::Spec,
            false,
            &cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        sqlx::query(
            r#"INSERT INTO card_codex_threads
                  (thread_id, card_id, role, wave_id, created_at, updated_at)
               VALUES ('thread-from-table', ?1, 'spec', ?2, 1000, 1000)"#,
        )
        .bind(card.id.as_str())
        .bind(wave.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let rows = spec_cards_for_boot_takeover_table_first(&repo)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, card.id.as_str());
        assert_eq!(rows[0].1, wave.id.as_str());
        assert_eq!(rows[0].2, "thread-from-table");
        assert_eq!(rows[0].3, Some(1234));
        assert_eq!(rows[0].4.as_deref(), Some("/tmp/spec.sock"));
        assert_eq!(rows[0].7, 42);
    }
}

/// Per-wave outcome of [`takeover_spec_appservers_on_boot`].
///
/// #313 PR4-round2 (B2): the previous `Reused` variant for *adopting* a
/// still-live persisted app-server was removed. Adopting safely required
/// either a `thread/status` probe (no such method on codex JSON-RPC) or
/// a pessimistic phase + reconciliation timer; the simpler correctness
/// fix is to ALWAYS respawn. The rare case where the prior server
/// survived a kernel SIGKILL (reparented under `systemd --user`) is now
/// reaped via `signal_process_group(pgid, …)` before the respawn so the
/// new server can rebind the socket.
#[derive(Debug, Clone, Copy)]
enum TakeoverOutcome {
    /// We spawned a fresh app-server and ran `initialize` + `thread/resume`
    /// against it. The previous persisted process group (if any) was
    /// reaped on the way in.
    Respawned,
    /// The wave is left without a live push channel. Either resume
    /// returned `-32600` (no rollout — payload cleared) or the
    /// spawn/connect/handshake errored. The dispatcher's missing-handle
    /// path will warn on the next live event and move on.
    Inert,
}

#[allow(clippy::too_many_arguments)]
async fn bootstrap_empty_goal_spec_appserver(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    cwd: &str,
    persisted_pgid: Option<i32>,
    persisted_sock: Option<&str>,
    persisted_start_time: Option<u64>,
    persisted_boot_id: Option<&str>,
    watermark: i64,
) -> TakeoverOutcome {
    if let (Some(pgid), Some(sock)) = (persisted_pgid, persisted_sock) {
        let sock_path = std::path::Path::new(sock);
        let identity_ok = match (persisted_start_time, persisted_boot_id) {
            (Some(st), Some(boot)) => spec_appserver::verify_owned_pid(pgid, st, boot),
            _ => false,
        };
        let socket_live = if identity_ok && pgid > 1 {
            spec_appserver::socket_owned_by_appserver(sock_path).await
        } else {
            false
        };
        if pgid > 1 && identity_ok && socket_live {
            tracing::debug!(
                card_id, wave_id = %wave_id, pgid, sock,
                "initial-prompt bootstrap: reaping stale rollout-less app-server before fresh spawn",
            );
            spec_appserver::signal_process_group(pgid, libc::SIGTERM);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if let (Some(st), Some(boot)) = (persisted_start_time, persisted_boot_id)
                && spec_appserver::verify_owned_pid(pgid, st, boot)
            {
                spec_appserver::signal_process_group(pgid, libc::SIGKILL);
            }
        } else if pgid > 1 {
            let cause = SkipKillCause::classify(
                persisted_start_time,
                persisted_boot_id,
                identity_ok,
                socket_live,
            );
            cause.emit(
                card_id,
                wave_id,
                pgid,
                sock,
                identity_ok,
                socket_live,
                persisted_start_time,
                persisted_boot_id,
            );
        }
        spec_appserver::cleanup_sock_dir(sock_path);
    }

    let env_map = crate::spec_card::build_codex_env_map(
        state.codex.as_ref(),
        card_id,
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
        None,
        None,
    );
    let sock = state.daemon.appserver_sock_path(card_id);
    let sock_dir = state.daemon.appserver_sock_dir(card_id);
    if let Err(e) = std::fs::create_dir_all(&sock_dir) {
        tracing::warn!(
            card_id, wave_id = %wave_id, error = %e,
            "initial-prompt bootstrap: mkdir appserver sock dir failed; leaving wave retryable",
        );
        return TakeoverOutcome::Inert;
    }
    let recovery_signal =
        wire_spec_push_recovery_supervisor(state, settings, card_id, wave_id.clone());
    // Empty-goal bootstrap respawns a fresh app-server (the prior thread had
    // no rollout and must never be resumed). The remote TUI will fresh-start
    // the thread using the spec developer_instructions seeded into this
    // card's CODEX_HOME/config.toml.
    let handle =
        match spec_appserver::spawn_spec_appserver_with_watchdog_config_and_recovery_for_wave(
            &state.codex.codex_bin,
            &env_map,
            "",
            &sock,
            None,
            spec_appserver::TurnWatchdogConfig::default(),
            Some(recovery_signal),
            Some(wave_id),
        )
        .await
        {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!(
                    card_id, wave_id = %wave_id, error = %e,
                    "initial-prompt bootstrap: fresh app-server spawn failed; leaving wave retryable",
                );
                return TakeoverOutcome::Inert;
            }
        };
    if let Err(e) = state
        .repo
        .spec_card_set_empty_goal_bootstrap_pending_state(
            card_id,
            handle.pgid,
            &handle.sock.to_string_lossy(),
            handle.start_time,
            handle.boot_id.as_deref(),
            watermark,
        )
        .await
    {
        tracing::warn!(
            card_id, wave_id = %wave_id, error = %e,
            "initial-prompt bootstrap: persist fresh runtime state failed; leaving wave retryable",
        );
        return TakeoverOutcome::Inert;
    }
    let push = crate::spec_card::SpecPushDaemonArgs {
        thread_id: None,
        sock: handle.sock.clone(),
    };
    register_and_catch_up(state, card_id, wave_id, watermark, handle, false).await;
    let mcp_token = env_map
        .get("NEIGE_MCP_TOKEN")
        .and_then(serde_json::Value::as_str);
    if let Err(e) = crate::spec_card::spawn_spec_daemon_for_existing_seed(
        state,
        card_id,
        wave_id.as_str(),
        cwd,
        &env_map,
        mcp_token,
        &push,
    )
    .await
    {
        tracing::warn!(
            card_id, wave_id = %wave_id, error = %e,
            "initial-prompt bootstrap: app-server parked but TUI daemon spawn failed",
        );
    }
    tracing::info!(
        card_id, wave_id = %wave_id,
        "initial-prompt bootstrap: fresh idle app-server registered without thread/resume",
    );
    TakeoverOutcome::Respawned
}

const RUNTIME_RECOVERY_MAX_RESTARTS: u32 = 3;
const RUNTIME_RECOVERY_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Clone, Copy)]
struct RuntimeRecoveryBudget {
    restart_count: u32,
    window_started: std::time::Instant,
}

impl Default for RuntimeRecoveryBudget {
    fn default() -> Self {
        Self {
            restart_count: 0,
            window_started: std::time::Instant::now(),
        }
    }
}

/// Wire one app-server handle's notification consumer to a runtime recovery
/// supervisor. The returned sender is passed into `spawn/resume_spec_appserver`;
/// the spawned task owns the receiver plus the `AppState`/card/wave/settings
/// context required to reuse the same rehydrate + catch-up path as boot
/// takeover.
pub(crate) fn wire_spec_push_recovery_supervisor(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: crate::ids::WaveId,
) -> spec_appserver::SpecRecoverySignal {
    wire_spec_push_recovery_supervisor_with_budget(
        state,
        settings,
        card_id,
        wave_id,
        RuntimeRecoveryBudget::default(),
        spec_appserver::TurnWatchdogConfig::default(),
    )
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn wire_spec_push_recovery_supervisor_for_test(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: crate::ids::WaveId,
) -> spec_appserver::SpecRecoverySignal {
    wire_spec_push_recovery_supervisor(state, settings, card_id, wave_id)
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn wire_spec_push_recovery_supervisor_with_watchdog_for_test(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: crate::ids::WaveId,
    watchdog: spec_appserver::TurnWatchdogConfig,
) -> spec_appserver::SpecRecoverySignal {
    wire_spec_push_recovery_supervisor_with_budget(
        state,
        settings,
        card_id,
        wave_id,
        RuntimeRecoveryBudget::default(),
        watchdog,
    )
}

fn wire_spec_push_recovery_supervisor_with_budget(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: crate::ids::WaveId,
    budget: RuntimeRecoveryBudget,
    watchdog: spec_appserver::TurnWatchdogConfig,
) -> spec_appserver::SpecRecoverySignal {
    let (signal, rx) = spec_appserver::recovery_signal_channel(wave_id.clone());
    let ctx = RuntimeRecoveryContext {
        state: state.clone(),
        settings: settings.clone(),
        card_id: card_id.to_string(),
        wave_id,
        budget,
        watchdog,
    };
    tokio::spawn(runtime_spec_push_recovery_supervisor(ctx, rx));
    signal
}

struct RuntimeRecoveryContext {
    state: state::AppState,
    settings: crate::routes::settings::Settings,
    card_id: String,
    wave_id: crate::ids::WaveId,
    budget: RuntimeRecoveryBudget,
    watchdog: spec_appserver::TurnWatchdogConfig,
}

async fn runtime_spec_push_recovery_supervisor(
    ctx: RuntimeRecoveryContext,
    mut rx: tokio::sync::mpsc::Receiver<spec_appserver::SpecRecoveryRequest>,
) {
    let Some(request) = rx.recv().await else {
        return;
    };
    if request.wave_id != ctx.wave_id {
        tracing::warn!(
            expected_wave = %ctx.wave_id,
            request_wave = %request.wave_id,
            "spec push runtime recovery: ignoring request for unexpected wave"
        );
        return;
    }

    let now = std::time::Instant::now();
    let mut budget = ctx.budget;
    if now.duration_since(budget.window_started) > RUNTIME_RECOVERY_WINDOW {
        budget.restart_count = 0;
        budget.window_started = now;
    }
    if budget.restart_count >= RUNTIME_RECOVERY_MAX_RESTARTS {
        tracing::error!(
            card_id = %ctx.card_id,
            wave_id = %ctx.wave_id,
            thread_id = %request.thread_id,
            turn_id = %request.turn_id,
            ?request.reason,
            restart_count = budget.restart_count,
            window_secs = RUNTIME_RECOVERY_WINDOW.as_secs(),
            "spec push runtime recovery: restart budget exhausted; leaving wave wedged/abandoned"
        );
        // Runtime recovery exhausted its restart budget; mark the wave abandoned.
        emit_spec_push_abandoned(&ctx.state, &ctx.wave_id).await;
        return;
    }

    let watermark = match current_spec_push_watermark(&ctx.state, &ctx.card_id, &ctx.wave_id).await
    {
        Some(watermark) => watermark,
        None => {
            tracing::warn!(
                card_id = %ctx.card_id,
                wave_id = %ctx.wave_id,
                "spec push runtime recovery: wave is no longer an in-flight spec takeover candidate; abandoning recovery"
            );
            // Durable lookup returned no takeover candidate; the wave is gone/terminal.
            emit_spec_push_abandoned(&ctx.state, &ctx.wave_id).await;
            return;
        }
    };

    tracing::warn!(
        card_id = %ctx.card_id,
        wave_id = %ctx.wave_id,
        thread_id = %request.thread_id,
        turn_id = %request.turn_id,
        ?request.reason,
        next_restart_count = budget.restart_count + 1,
        "spec push runtime recovery: reaping wedged app-server and resuming fresh process"
    );
    crate::terminal_sweeper::reap_spec_push(&ctx.state, &ctx.wave_id).await;

    let next_budget = RuntimeRecoveryBudget {
        restart_count: budget.restart_count + 1,
        window_started: budget.window_started,
    };
    let outcome = resume_and_register_spec_appserver(
        &ctx.state,
        &ctx.settings,
        &ctx.card_id,
        &ctx.wave_id,
        &request.thread_id,
        watermark,
        Some(next_budget),
        true,
        ctx.watchdog,
        "runtime recovery",
    )
    .await;
    if matches!(outcome, TakeoverOutcome::Inert) {
        tracing::warn!(
            card_id = %ctx.card_id,
            wave_id = %ctx.wave_id,
            "spec push runtime recovery: resume/register failed; wave left inert"
        );
    }
}

async fn current_spec_push_watermark(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
) -> Option<i64> {
    let rows = match state.repo.spec_cards_for_boot_takeover().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                card_id,
                wave_id = %wave_id,
                error = %e,
                "spec push runtime recovery: failed to read current push watermark"
            );
            return None;
        }
    };
    rows.into_iter().find_map(
        |(row_card, row_wave, _thread, _pgid, _sock, _start, _boot, watermark)| {
            if row_card == card_id && row_wave == wave_id.as_str() {
                Some(watermark)
            } else {
                None
            }
        },
    )
}

/// #328 P2 (log split) — distinct reasons we skip the kill of a persisted
/// app-server pgid at boot takeover. The pre-#328 path emitted a single
/// `warn!` whose message lumped three causes together; structured fields
/// still distinguished them but a human reading the message saw one
/// blurry warning. SRE triage now reads the message and gets the cause
/// in plain English, with the structured fields preserved for the
/// field-readers (alerts, log queries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipKillCause {
    /// Pre-#318 spec card row — `start_time` and/or `boot_id` were
    /// never persisted, so identity can't be proven. Conservative
    /// posture (matches pre-#318 behavior for legacy rows): skip the
    /// kill, clean the socket, respawn. Will not recur for cards
    /// created post-#318.
    MissingStamp,
    /// Stamps present but `verify_owned_pid` rejected: either the host
    /// rebooted (`boot_id` differs), the pid is gone (`/proc/<pid>`
    /// ENOENT), or a same-boot pid recycle landed (`starttime`
    /// mismatch). In every case the persisted pgid is NOT our
    /// app-server.
    IdentityMismatch,
    /// Identity proved but the socket probe failed — the persisted pid
    /// is still alive and ours, but isn't listening on the per-card
    /// socket path. Likely a crash mid-accept leaving a stale socket
    /// dirent; SIGKILLing a zombie that wasn't going to interfere with
    /// `bind(2)` anyway is strictly worse than just respawning.
    StaleSocketDirent,
}

impl SkipKillCause {
    fn classify(
        persisted_start_time: Option<u64>,
        persisted_boot_id: Option<&str>,
        identity_ok: bool,
        socket_live: bool,
    ) -> Self {
        if persisted_start_time.is_none() || persisted_boot_id.is_none() {
            Self::MissingStamp
        } else if !identity_ok {
            Self::IdentityMismatch
        } else {
            // identity_ok = true here; the only remaining skip reason is
            // !socket_live (otherwise we'd be on the kill path).
            debug_assert!(
                !socket_live,
                "SkipKillCause::classify reached the stale-dirent arm with \
                 socket_live=true — caller should have fired the kill"
            );
            Self::StaleSocketDirent
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        self,
        card_id: &str,
        wave_id: &crate::ids::WaveId,
        pgid: i32,
        sock: &str,
        identity_ok: bool,
        socket_live: bool,
        persisted_start_time: Option<u64>,
        persisted_boot_id: Option<&str>,
    ) {
        match self {
            Self::MissingStamp => {
                tracing::info!(
                    card_id, wave_id = %wave_id, pgid, sock,
                    identity_ok, socket_live,
                    start_time = ?persisted_start_time,
                    boot_id = ?persisted_boot_id,
                    "takeover: skipping kill of persisted pgid — \
                     pre-#318 spec card row lacks start_time/boot_id stamp; \
                     can't prove identity, cleaning stale socket and respawning"
                );
            }
            Self::IdentityMismatch => {
                tracing::info!(
                    card_id, wave_id = %wave_id, pgid, sock,
                    identity_ok, socket_live,
                    start_time = ?persisted_start_time,
                    boot_id = ?persisted_boot_id,
                    "takeover: skipping kill of persisted pgid — \
                     identity check failed (host reboot, pid recycle, or \
                     process gone); cleaning stale socket and respawning"
                );
            }
            Self::StaleSocketDirent => {
                tracing::info!(
                    card_id, wave_id = %wave_id, pgid, sock,
                    identity_ok, socket_live,
                    start_time = ?persisted_start_time,
                    boot_id = ?persisted_boot_id,
                    "takeover: skipping kill of persisted pgid — \
                     identity ok but socket probe failed (stale dirent / \
                     frozen accept loop); SIGKILLing a non-listening own \
                     process wouldn't help bind(2), cleaning stale socket \
                     and respawning"
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_takeover_one_wave(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    thread_id: &str,
    persisted_pgid: Option<i32>,
    persisted_sock: Option<&str>,
    persisted_start_time: Option<u64>,
    persisted_boot_id: Option<&str>,
    watermark: i64,
) -> TakeoverOutcome {
    // 1. #313 PR4-round2 (B2): **always respawn**. We unconditionally reap
    //    any persisted process group and clean the stale socket before
    //    `resume_spec_appserver`. The previous adopt path (re-attach to
    //    a still-listening server) was removed: safe adoption needed
    //    either a `thread/status` probe (no such method on codex
    //    JSON-RPC) or a pessimistic phase + reconciliation timer, both
    //    adding complexity for a marginal optimization. Worse, the
    //    round-1 adopt seeded the handle as `Idle` and boot catch-up
    //    fired a `turn/start` against a possibly-mid-turn server →
    //    codex silently dropped the catch-up envelope (the very bug the
    //    push queue exists to prevent).
    //
    //    The reap below is best-effort: `signal_process_group` is a
    //    no-op (ESRCH) if the group is already gone (graceful teardown
    //    on prior shutdown). It uses the negative-pgid form so the
    //    whole group goes down, not just the leader — fixing the
    //    earlier hazard where the native `codex app-server` child
    //    (reparented under `systemd --user`) survived a leader-only
    //    SIGKILL and kept the socket bound.
    if let (Some(pgid), Some(sock)) = (persisted_pgid, persisted_sock) {
        let sock_path = std::path::Path::new(sock);
        // #318 INV-5 (R3-B1) — STRONG PID OWNERSHIP CHECK before kill.
        //
        // Round-3 of #313 gated the kill on `socket_owned_by_appserver`
        // (a `UnixStream::connect` to the per-card socket path). That's
        // good for the steady-state (a connectable listener at our
        // UUID-scoped path is overwhelmingly ours), but suffers a TOCTOU
        // window between the probe returning `true` and the
        // `signal_process_group` syscall fired ~400 ms later (SIGTERM →
        // grace → SIGKILL). Inside that window the original listener
        // can exit, its pid/pgid can be recycled by the kernel, and our
        // SIGTERM/SIGKILL then lands on an unrelated process group.
        //
        // The fix is `(pid, start_time, boot_id)` identity: we captured
        // the launcher's `starttime` (`/proc/<pgid>/stat` field 22,
        // jiffies-since-boot) AND the kernel's `boot_id`
        // (`/proc/sys/kernel/random/boot_id`) at spawn and persisted
        // both on the spec card payload. `verify_owned_pid` rejects on
        // ANY of:
        //   * `boot_id` mismatch → host rebooted → prior boot's pid
        //     namespace is dead in its entirety, regardless of any
        //     pid's stamp.
        //   * `/proc/<pid>` ENOENT → process is gone.
        //   * `starttime` mismatch → same-boot pid recycle, recycled
        //     process started after our stamp.
        //
        // We require BOTH identity (via `verify_owned_pid`) AND socket
        // liveness (via `socket_owned_by_appserver`) to fire the kill.
        // Belt-and-suspenders: identity proves "this pid is ours";
        // socket-owned proves "and it's still listening on our path".
        // Either alone is a stronger guarantee than pre-#318, but
        // requiring both closes the residual gap where identity check
        // passes against a same-boot ours-but-frozen process (we
        // crashed mid-accept, the socket dirent is stale, and we're
        // about to respawn) — we'd be SIGKILL'ing a zombie that
        // wouldn't have interfered with `bind(2)` anyway, so skipping
        // is strictly safer.
        //
        // Decision matrix:
        //   * identity_ok AND socket_live → fire SIGTERM → grace →
        //     SIGKILL → cleanup.
        //   * identity_ok AND NOT socket_live → skip kill. The
        //     persisted pid is alive and ours but not listening; the
        //     respawn's `bind(2)` will succeed once we
        //     `cleanup_sock_dir`. (Rare leak: one stale process group
        //     survives until host shutdown or manual cleanup. Benign.)
        //   * NOT identity_ok → skip kill regardless of socket. The
        //     persisted pid is dead / belongs to someone else.
        //   * persisted stamp/boot_id absent (`None`) → can't prove
        //     identity → skip kill. Conservative; same posture as
        //     pre-#318 for legacy rows.
        //
        // Either way we still call `cleanup_sock_dir` so the new
        // app-server can rebind a fresh socket file.
        let identity_ok = match (persisted_start_time, persisted_boot_id) {
            (Some(st), Some(boot)) => spec_appserver::verify_owned_pid(pgid, st, boot),
            _ => false,
        };
        let socket_live = if identity_ok && pgid > 1 {
            spec_appserver::socket_owned_by_appserver(sock_path).await
        } else {
            false
        };
        if pgid > 1 && identity_ok && socket_live {
            tracing::debug!(
                card_id, wave_id = %wave_id, pgid, sock,
                start_time = ?persisted_start_time,
                boot_id = ?persisted_boot_id,
                "takeover: pid identity AND socket liveness both verified — \
                 reaping persisted app-server process group before respawn"
            );
            spec_appserver::signal_process_group(pgid, libc::SIGTERM);
            // Brief grace so the launcher can flush before SIGKILL.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            // #328 P2 (re-verify identity before SIGKILL escalation) —
            // TOCTOU defense. The first `verify_owned_pid` above was 200 ms
            // ago; between SIGTERM landing and SIGKILL firing, the group
            // can exit and the kernel can recycle its pgid to an unrelated
            // user process. If that happened, our SIGKILL would land on
            // that innocent process. Re-run the `(pid, start_time,
            // boot_id)` triple check; skip SIGKILL if the live process at
            // `pgid` is no longer ours (either reaped + recycled, or
            // simply reaped — `verify_owned_pid` also returns false on
            // ENOENT, which is the common case after a successful
            // SIGTERM).
            //
            // We keep both stamps in scope here (already unwrapped on the
            // outer match), so the re-verify is a single proc-stat read
            // + boot-id read — cheap relative to the syscall it gates.
            let still_ours = match (persisted_start_time, persisted_boot_id) {
                (Some(st), Some(boot)) => spec_appserver::verify_owned_pid(pgid, st, boot),
                _ => false,
            };
            if still_ours {
                spec_appserver::signal_process_group(pgid, libc::SIGKILL);
            } else {
                tracing::debug!(
                    card_id, wave_id = %wave_id, pgid, sock,
                    start_time = ?persisted_start_time,
                    boot_id = ?persisted_boot_id,
                    "takeover: skipping SIGKILL escalation — identity no \
                     longer matches after SIGTERM grace (process exited or \
                     pgid was recycled); SIGTERM already did the job or \
                     the recycled target is not ours to kill"
                );
            }
        } else if pgid > 1 {
            // #328 P2 (log split) — one warn covering three distinct causes
            // makes SRE triage harder than it needs to be. Classify into a
            // small enum and emit a cause-specific message; structured
            // fields stay on every variant for the field-readers.
            let cause = SkipKillCause::classify(
                persisted_start_time,
                persisted_boot_id,
                identity_ok,
                socket_live,
            );
            cause.emit(
                card_id,
                wave_id,
                pgid,
                sock,
                identity_ok,
                socket_live,
                persisted_start_time,
                persisted_boot_id,
            );
        }
        spec_appserver::cleanup_sock_dir(sock_path);
    }

    // 2. Respawn/register path shared with runtime wedge recovery.
    resume_and_register_spec_appserver(
        state,
        settings,
        card_id,
        wave_id,
        thread_id,
        watermark,
        Some(RuntimeRecoveryBudget::default()),
        false,
        spec_appserver::TurnWatchdogConfig::default(),
        "takeover",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn resume_and_register_spec_appserver(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    thread_id: &str,
    watermark: i64,
    recovery_budget: Option<RuntimeRecoveryBudget>,
    reset_cursor_to_watermark: bool,
    watchdog: spec_appserver::TurnWatchdogConfig,
    log_prefix: &'static str,
) -> TakeoverOutcome {
    // Build the env the way `create_wave` does, point at the per-card
    // socket path (same resolver as the route), and run
    // `resume_spec_appserver` (the create-wave shape, swapping
    // thread/start + turn/start + initial lifecycle wait for thread/resume).
    let env_map = crate::spec_card::build_codex_env_map(
        state.codex.as_ref(),
        card_id,
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
        // No MCP env on respawn: the per-card `$CODEX_HOME/config.toml`
        // already bakes `NEIGE_MCP_TOKEN`/`NEIGE_MCP_SOCKET` into the
        // `[mcp_servers.calm].env` block, and we don't have the raw token
        // (only the hash is persisted). Codex picks the values up from
        // the config block when it spawns its MCP transport — same path
        // a live respawned server takes today.
        None,
        None,
    );
    let sock = state.daemon.appserver_sock_path(card_id);
    let sock_dir = state.daemon.appserver_sock_dir(card_id);
    if let Err(e) = std::fs::create_dir_all(&sock_dir) {
        tracing::warn!(
            card_id, wave_id = %wave_id, error = %e,
            "{log_prefix}: mkdir appserver sock dir failed; leaving wave inert",
        );
        // #318 INV-1 (b) — abandonment signal for the no-skip review gap
        // from #315. Mkdir failure means we never even attempted resume,
        // so events with `id > push_watermark` for this wave will sit
        // stranded; emit so SRE / future re-run path (#313 problem #2)
        // sees a durable record.
        emit_spec_push_abandoned(state, wave_id).await;
        return TakeoverOutcome::Inert;
    }
    let recovery_signal = recovery_budget.map(|budget| {
        wire_spec_push_recovery_supervisor_with_budget(
            state,
            settings,
            card_id,
            wave_id.clone(),
            budget,
            watchdog,
        )
    });
    match spec_appserver::resume_spec_appserver_with_watchdog_config_and_recovery(
        &state.codex.codex_bin,
        &env_map,
        thread_id,
        &sock,
        watchdog,
        recovery_signal,
    )
    .await
    {
        Ok(handle) => {
            tracing::info!(
                card_id, wave_id = %wave_id, thread_id,
                "{log_prefix}: respawned codex app-server + thread/resume succeeded",
            );
            // Persist the fresh pgid + sock + (start_time, boot_id) for
            // the NEXT boot cycle so a hard-crash between this point and
            // the next graceful teardown can verify the persisted pgid's
            // identity (#318 INV-5) AND probe its socket against the new
            // process. (Same write `create_wave` does post-spawn, minus
            // the codex_thread_id which is already persisted.)
            persist_post_respawn_fields(
                state,
                card_id,
                handle.pgid,
                &handle.sock.to_string_lossy(),
                handle.start_time,
                handle.boot_id.as_deref(),
            )
            .await;
            register_and_catch_up(
                state,
                card_id,
                wave_id,
                watermark,
                handle,
                reset_cursor_to_watermark,
            )
            .await;
            TakeoverOutcome::Respawned
        }
        Err(e) => {
            // Classify the failure: `-32600 "no rollout found"` means the
            // wave never completed turn #1 last boot, so the rollout file
            // doesn't exist on disk and no respawn can ever resume it.
            // Clear the stale push fields so the next boot stops retrying
            // — the wave is inert until issue #313 problem #2 wires up a
            // re-run path (out of scope).
            //
            // #313 problem #1 round-3 (N3) — tighten the classifier to
            // require BOTH "no rollout" AND "-32600". The earlier OR form
            // would clear push state for *any* -32600 error (codex uses
            // -32600 for several invalid-request shapes), which could
            // wedge a wave whose rollout actually still exists. Both
            // tokens together are codex's specific phrasing for the
            // missing-rollout case.
            let msg = e.to_string();
            let no_rollout = msg.contains("no rollout") && msg.contains("-32600");
            if no_rollout {
                tracing::warn!(
                    card_id, wave_id = %wave_id, thread_id, error = %msg,
                    "{log_prefix}: thread/resume returned -32600 no rollout; clearing stale push state — wave inert until manual restart (#313 problem #2)",
                );
                if let Err(e2) = state.repo.spec_card_clear_push_state(card_id).await {
                    tracing::warn!(
                        card_id, error = %e2,
                        "takeover: spec_card_clear_push_state failed (best-effort)",
                    );
                }
            } else {
                tracing::warn!(
                    card_id, wave_id = %wave_id, thread_id, error = %msg,
                    "{log_prefix}: respawn app-server / resume failed; leaving wave inert (next boot retries)",
                );
            }
            // #318 INV-1 (b) — abandonment signal. Both inert sub-paths
            // here (resume returned `-32600 "no rollout"`, or spawn /
            // connect / handshake errored) leave the wave without a
            // live push channel, so any persisted event with `id >
            // push_watermark` for this wave is stranded until manual
            // re-run wiring lands (#313 problem #2). Emit unconditionally
            // for every Inert path so the signal isn't conditional on
            // the classifier sub-branch — the consumer downstream
            // doesn't care WHY we abandoned, only THAT we did.
            emit_spec_push_abandoned(state, wave_id).await;
            TakeoverOutcome::Inert
        }
    }
}

/// #318 INV-1 (b) — emit [`Event::SpecPushAbandoned`] for a wave that
/// boot takeover gave up on. Centralized here so every
/// [`TakeoverOutcome::Inert`] exit point in [`try_takeover_one_wave`]
/// routes through the same persistence + broadcast call: the spec card
/// is now excluded from `spec_cards_for_boot_takeover` on future boots
/// (no `codex_thread_id` after clear, OR resume keeps failing), so this
/// is the only durable record that SRE / future re-run code will see
/// for the stranded envelopes.
///
/// **Why `log_pure_event` and not `EventBus::emit`**: the abandonment
/// must survive a kernel restart that happens between this call and a
/// human reading it — otherwise the signal is no more observable than
/// the existing `tracing::warn!`. `log_pure_event` persists the row in
/// the events table BEFORE broadcasting, so a subscriber catching up
/// via `events_since(cursor)` after a future restart still sees the
/// signal.
///
/// **`last_envelope_id` semantics**: pulled from
/// [`crate::db::RepoEventWrite::events_latest_id_for_wave`] — the
/// largest `events.id` whose `scope_wave` matches this wave at the
/// moment of abandonment. Upper bound on the stranded set: every id in
/// `(push_watermark, last_envelope_id]` for this wave is at risk.
/// `None` (no wave-scoped rows yet — abandonment happened before any
/// event was emitted in scope) maps to `0`, the `events.id` "no row"
/// sentinel. Callers that want the *exact* stranded set can run their
/// own `events_since(push_watermark)` filtered to this wave_id; this
/// payload field is a cheap upper bound for sizing.
///
/// **`cove_id` resolution**: via [`crate::wave_cove_cache::WaveCoveCache`]
/// (write-through cache seeded at boot from `waves.cove_id`). A miss
/// here means the wave row was deleted between
/// `spec_cards_for_boot_takeover` returning the row and this point —
/// which would also fail the SQL JOIN in `spec_cards_for_boot_takeover`,
/// so it's effectively unreachable. We log + return without emitting
/// rather than fabricating a sentinel cove_id; the wave is gone, the
/// signal would route to no live subscriber, and the consumer can rely
/// on cove_id being authoritative.
///
/// Persistence + broadcast failure is logged at `warn!` and otherwise
/// swallowed — boot stays best-effort (one wave's signal failing must
/// not skip takeover for the next wave). The persisted event itself
/// is the durability boundary; if the write fails the wave is still
/// inert from `tracing::warn!`'s perspective.
async fn emit_spec_push_abandoned(state: &state::AppState, wave_id: &crate::ids::WaveId) {
    let Some(cove_id) = state.wave_cove_cache.cove_of(wave_id) else {
        // Wave row deleted between the takeover-input SELECT and this
        // emit — the JOIN in `spec_cards_for_boot_takeover` would have
        // filtered it out, so in practice this branch is unreachable.
        // Log loudly so a future regression in cache seeding (or a new
        // takeover entry-point that doesn't go through the boot SELECT)
        // shows up here.
        tracing::warn!(
            wave_id = %wave_id,
            "takeover: skipping SpecPushAbandoned emit — wave_cove_cache miss \
             (wave row deleted concurrently?)"
        );
        return;
    };
    let last_envelope_id = match state.repo.events_latest_id_for_wave(wave_id.as_str()).await {
        Ok(opt) => opt.unwrap_or(0),
        Err(e) => {
            // SELECT failure shouldn't block the signal — emit with the
            // `0` sentinel and log the underlying error. Consumers that
            // want the exact set can re-query off the `wave_id` topic.
            tracing::warn!(
                wave_id = %wave_id, error = %e,
                "takeover: events_latest_id_for_wave failed; \
                 emitting SpecPushAbandoned with last_envelope_id = 0"
            );
            0
        }
    };
    let event = crate::event::Event::SpecPushAbandoned {
        wave_id: wave_id.clone(),
        cove_id: cove_id.clone(),
        last_envelope_id,
    };
    let scope = crate::event::EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id,
    };
    if let Err(e) = state
        .repo
        .log_pure_event(
            crate::ids::ActorId::Kernel,
            scope,
            None,
            &state.events,
            &state.card_role_cache,
            &state.wave_cove_cache,
            event,
        )
        .await
    {
        tracing::warn!(
            wave_id = %wave_id, error = %e,
            "takeover: log_pure_event(SpecPushAbandoned) failed; \
             wave is still inert but signal was not persisted"
        );
    } else {
        tracing::info!(
            wave_id = %wave_id, last_envelope_id,
            "takeover: emitted SpecPushAbandoned (#318 INV-1 b)"
        );
    }
}

/// Persist the freshly-respawned app-server's pgid + sock back onto the
/// spec card's payload via a small JSON-merge UPDATE. Identical shape to
/// the create-wave persist, minus `codex_thread_id` (already on the row)
/// and `push_watermark` (already on the row — we MUST NOT clobber it).
async fn persist_post_respawn_fields(
    state: &state::AppState,
    card_id: &str,
    pgid: i32,
    sock: &str,
    start_time: Option<u64>,
    boot_id: Option<&str>,
) {
    // Same pattern as `spec_card_set_push_watermark`: a single-statement
    // JSON-merge UPDATE that touches only the named keys. Going through
    // `write_with_event_typed` would emit a `CardUpdated` event for what
    // is purely kernel-internal bookkeeping — same reason terminal PIDs /
    // handles go through `RepoOutOfDomain` instead.
    if let Err(e) = state
        .repo
        .spec_card_set_appserver_after_takeover(card_id, pgid, sock, start_time, boot_id)
        .await
    {
        tracing::warn!(
            card_id, error = %e,
            "takeover: persist post-respawn pgid+sock+identity failed; in-memory handle is parked, next boot will probe stale fields",
        );
    }
}

/// Register the resumed [`SpecPushHandle`] in the registry and catch the
/// spec thread up with every event `id > watermark` for this wave via the
/// dispatcher's normal push path.
///
/// #313 problem #1 round-2 (B3) — the whole sequence
/// `(seed_push_cursor → install_watermark_sink → spec_push.insert →
/// catch_up_push for every event)` runs **under the dispatcher's per-wave
/// push lock**. Live `Inner::push_to_spec` paths take the same lock, so a
/// `task.completed`/`task.failed`/`wave.report_edited` arriving on the
/// broadcast bus while takeover is mid-catch-up serializes behind it
/// instead of slipping past the seeded watermark. Without this guard, a
/// live event landing in the window between `insert` and the final
/// `catch_up_push` could:
///   * see the freshly-seeded cursor,
///   * `bump` it to its own envelope id,
///   * try to resolve the handle — race against `insert`,
///   * and `events_since(watermark)` (already evaluated against the
///     pre-bump watermark in this fn) would then NOT see ids between the
///     bump and the live event, losing them.
async fn register_and_catch_up(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    handle: spec_appserver::SpecPushHandle,
    reset_cursor_to_watermark: bool,
) {
    let card_key: crate::ids::CardId = card_id.to_string().into();

    // #315 round-4 (B1 defence-in-depth) — the SQL filter in
    // `spec_cards_for_boot_takeover` is the primary guard against a
    // non-spec card's payload field colliding with the takeover query
    // (see `db/sqlite.rs::spec_cards_for_boot_takeover` for the
    // rationale + role predicate). This `debug_assert!` re-checks the
    // invariant against the in-memory role cache so any future query
    // refactor that drops the role predicate (or any new takeover
    // entry-point) fails fast in dev/test rather than silently
    // registering the wrong handle for a wave. The cache is seeded from
    // the persisted `cards.role` column at boot
    // (`seed_card_role_cache`); a card that reached this fn without
    // role = Spec means either (a) the SQL filter regressed or (b) the
    // cache is stale (likely a bug in `card_create_with_id_tx`'s
    // write-through). Either way, abort early in test runs — production
    // builds elide the check.
    debug_assert!(
        state
            .card_role_cache
            .get(&card_key)
            .is_none_or(|role| role == crate::model::CardRole::Spec),
        "register_and_catch_up: card {card_id:?} is not CardRole::Spec; \
         the boot-takeover query MUST scope to spec-role cards \
         (see spec_cards_for_boot_takeover)"
    );

    // B1 — install the watermark sink on the handle BEFORE the handle is
    // parked in the registry, so the very first queue flush triggered by
    // a catch-up push hitting `Enqueue` has a persister to call.
    //
    // #313 problem #1 round-3 (N7) — `debug_assert!` symmetric with the
    // sister install site in `routes/waves.rs::spawn_push_appserver`. A
    // future refactor that splits this install from its site would fail
    // fast in dev/test rather than silently dropping flushed envelopes
    // from the watermark.
    let sink = state.dispatcher.watermark_sink_for(card_key.clone());
    handle.install_watermark_sink(sink).await;
    debug_assert!(
        handle.has_watermark_sink().await,
        "register_and_catch_up: install_watermark_sink did not take effect — \
         queued-then-flushed envelopes would silently fail to persist their watermark"
    );
    let initial_prompt_ready = if handle.thread_id.is_none() {
        match state.wave_cove_cache.cove_of(wave_id) {
            Some(cove_id) => state.dispatcher.initial_prompt_ready_sink_for(
                card_key.clone(),
                wave_id.clone(),
                cove_id,
            ),
            None => state
                .dispatcher
                .initial_prompt_clear_sink_for(card_key.clone()),
        }
    } else {
        state
            .dispatcher
            .initial_prompt_clear_sink_for(card_key.clone())
    };
    handle
        .install_initial_prompt_ready_sink(initial_prompt_ready)
        .await;

    // #318 INV-3 (R2-B1) — install the durable queue-persist callbacks
    // alongside the watermark sink, then rehydrate the in-memory queue
    // from any rows a prior process enqueued but didn't flush. Both steps
    // happen BEFORE the handle is parked in the registry below (under the
    // per-wave push lock) so the very first catch-up push has both the
    // persist path AND the rehydrated cache available.
    //
    // Sister install: `routes/waves.rs::spawn_push_appserver` (create-wave
    // path). INV-6 demands the two paths run the same init hook —
    // installing here keeps boot-takeover symmetric with create-wave.
    let persist = state.dispatcher.queue_persist_for(card_key.clone());
    handle.install_queue_persist(persist).await;
    debug_assert!(
        handle.has_queue_persist().await,
        "register_and_catch_up: install_queue_persist did not take effect — \
         enqueued-but-not-yet-flushed observations would not survive the \
         next process restart, silently reintroducing the INV-3 (#318) regression"
    );
    // #325 fix — capture the rehydrated envelope_ids so the catch-up
    // replay below can skip them. Without this skip-set, a crash AFTER the
    // `Enqueue` arm persisted its row but BEFORE the consumer's flush
    // advanced `push_watermark` leaves both (a) a row that rehydrates here
    // and (b) the same envelope id in `events_since(watermark)` (the
    // dispatcher cooperatively withholds `push_watermark` on `Enqueued` —
    // PR #315 PR4 B1 — exactly so the events log is a recovery safety
    // net). With both surfaces present, the first catch-up push would
    // trigger `StartTurnNow` on the resumed (Idle) handle, drain the
    // rehydrated row, AND append the catch-up envelope as a *second copy*
    // of the same observation — a duplicate to codex on every recovery.
    //
    // Dedup is by `envelope_id` (the persisted `events.id`) — the rehydrate
    // path reads ids straight off `spec_card_queued_observations`, which
    // are the same `events.id` values `events_since` returns, so equality
    // is exact.
    // #325 round-2 P2 — pass the watermark in so rehydrate can drop rows
    // whose `envelope_id <= watermark` (already delivered to codex on a
    // prior process — the flush succeeded and bumped the watermark, but
    // the `dequeue` write didn't commit). Those rows are physically
    // deleted from `spec_push_queue` inside `rehydrate_queue_from_persist`
    // so a third boot doesn't see them either, and only the live
    // (un-delivered) envelope_ids are returned for the catch-up
    // dedup skip-set.
    let rehydrated_ids = handle.rehydrate_queue_from_persist(watermark).await;
    let rehydrated_count = rehydrated_ids.len();
    if rehydrated_count > 0 {
        tracing::info!(
            card_id,
            wave_id = %wave_id,
            count = rehydrated_count,
            "takeover: rehydrated spec push queue from durable rows; \
             items will deliver on the next turn/completed flush",
        );
    }

    // B3 — hold the per-wave push lock for the WHOLE
    // `seed → insert → events_since → catch-up replay` sequence so any
    // live event landing on the bus during this window serializes behind
    // takeover at the `Inner::push_to_spec` site (it tries to take the
    // SAME `Arc<Mutex>`). Without this, a live event could:
    //   * see the freshly-seeded cursor (or worse, the un-seeded 0),
    //   * `bump` to its own envelope id,
    //   * persist watermark to its own id,
    //   * and our catch-up replays for ids ≤ the live event would dedup
    //     silently, losing every event between the persisted watermark
    //     and the live event.
    //
    // We use `catch_up_push_under_lock` (not the public `catch_up_push`)
    // inside the closure because `tokio::sync::Mutex` is not reentrant.
    //
    // CRITICAL: every state-mutating step — seed, insert, events_since,
    // replay — runs INSIDE the lock. Reading `events_since` OUTSIDE would
    // open a window where a live event lands between the read and the
    // lock acquisition: it'd take the lock first (we're awaiting the
    // SELECT), `bump` the (un-seeded) cursor to its own id, miss the
    // handle (not yet inserted), warn-and-return — and our subsequent
    // replay for ids ≤ that bump would dedup silently. By doing the SELECT
    // under the lock, a live push for the same wave blocks at our lock;
    // its own row IS in our snapshot (it was persisted before we ran
    // the SELECT, OR it lands during the replay window and serializes
    // behind us, in which case its own push_to_spec replays it correctly
    // after we release).
    state
        .dispatcher
        .with_push_lock(wave_id, async move {
            if reset_cursor_to_watermark {
                // Runtime recovery reuses the same dispatcher process. Its
                // soft cursor can be ahead of the durable watermark after
                // enqueue/list persistence failures; force it down before
                // event-log catch-up so undelivered rows are not deduped.
                state
                    .dispatcher
                    .reset_push_cursor_to_watermark(card_key, watermark);
            } else {
                // Boot takeover starts with a fresh in-memory cursor. Keep
                // the existing monotonic seed so boot cannot lower a cursor
                // that a serialized live push already advanced.
                state.dispatcher.seed_push_cursor(card_key, watermark);
            }

            // Register the handle — `Inner::push_to_spec` resolves on this.
            // Still under the per-wave lock, so any concurrent live event
            // for this wave waits at `Inner::push_to_spec`'s lock until
            // catch-up finishes.
            //
            // #322 — `park` (not the bare `insert`) runs the aspect
            // framework's `BeforeHandleParkInRegistry` checks first; INV-6
            // (`WatermarkSinkInstalledAspect`) panics in release if a
            // future refactor drops the `install_watermark_sink` call
            // above. The `debug_assert!` above is the local fast-fail at
            // the install site; the aspect is the framework-level
            // enforcement at the park site (belt + suspenders, both
            // pointing at INV-6).
            state
                .spec_push
                .park(wave_id.clone(), handle, state.aspects.as_ref())
                .await;

            replay_spec_push_catch_up_under_lock(
                state,
                card_id,
                wave_id,
                watermark,
                rehydrated_ids,
            )
            .await;
        })
        .await;
}

/// Rehydrate durable queue rows and replay event-log rows for a reset-created,
/// already-parked spec push handle. The caller must hold the per-wave push
/// lock; this helper intentionally uses `catch_up_push_under_lock`.
pub(crate) async fn rehydrate_and_catch_up_parked_spec_push_under_lock(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
) {
    let card_key: crate::ids::CardId = card_id.to_string().into();
    let rehydrated_ids = state
        .spec_push
        .rehydrate_queue_from_persist(wave_id, watermark)
        .await;
    let rehydrated_count = rehydrated_ids.len();
    if rehydrated_count > 0 {
        tracing::info!(
            card_id,
            wave_id = %wave_id,
            count = rehydrated_count,
            "reset: rehydrated spec push queue from durable rows",
        );
    }
    state
        .dispatcher
        .reset_push_cursor_to_watermark(card_key, watermark);
    replay_spec_push_catch_up_under_lock(state, card_id, wave_id, watermark, rehydrated_ids).await;
}

async fn replay_spec_push_catch_up_under_lock(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    rehydrated_ids: Vec<i64>,
) {
    let rehydrated_skip: std::collections::HashSet<i64> = rehydrated_ids.iter().copied().collect();
    let rehydrated_count = rehydrated_ids.len();
    let rows = match state.repo.events_since(watermark, None).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                card_id, wave_id = %wave_id, watermark, error = %e,
                "spec push catch-up: events_since failed; spec thread will only see new live events from here",
            );
            return;
        }
    };
    let mut replayed = 0usize;
    let mut skipped_rehydrated = 0usize;
    for (id, _ver, scope, ev) in rows {
        let Some(ev_wave) = scope.wave_id() else {
            continue;
        };
        if ev_wave != wave_id {
            continue;
        }
        if !dispatcher::event_warrants_spec_push(&ev, &state.card_role_cache) {
            continue;
        }
        if rehydrated_skip.contains(&id) {
            skipped_rehydrated += 1;
            continue;
        }
        state
            .dispatcher
            .catch_up_push_under_lock(wave_id.clone(), ev, id)
            .await;
        replayed += 1;
    }
    if replayed > 0 || skipped_rehydrated > 0 {
        tracing::info!(
            card_id, wave_id = %wave_id, replayed, skipped_rehydrated, watermark,
            "spec push catch-up: replay pushed events to spec thread",
        );
    } else {
        tracing::debug!(
            card_id, wave_id = %wave_id, watermark,
            "spec push catch-up: no events above watermark",
        );
    }

    if rehydrated_count > 0
        && replayed == 0
        && let Some(pusher) = state.spec_push.pusher(wave_id)
    {
        pusher.flush_pending().await;
    }
}

pub mod card_fsm;
pub mod card_role_cache;
pub mod codex_appserver;
pub mod codex_auto_submit;
pub mod config;
pub mod db;
pub mod dispatcher;
pub mod error;
pub mod event;
pub mod event_cursor;
pub mod ids;
pub mod mcp_server;
pub mod model;
pub mod openapi;
pub mod plugin_host;
pub(crate) mod proc_supervisor;
pub mod replay;
pub mod role_gate;
pub mod routes;
pub mod shared_codex_home;
pub mod spec_appserver;
pub mod spec_card;
pub mod state;
pub mod terminal_renderer;
pub mod terminal_sweeper;
pub mod validation;
pub mod wave_cove_cache;
pub mod wave_lifecycle;
pub mod wave_report;
pub mod wave_report_doc;
pub mod ws;
