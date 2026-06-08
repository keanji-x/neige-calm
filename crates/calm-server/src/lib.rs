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
pub mod card_kind;
pub mod harness;
use crate::runtime_repo::RunStatus;

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
                // Synthetic -1 means the process outcome is unknown at boot, so treat it as Exited.
                if let Err(e) = state
                    .repo
                    .runtime_complete_for_terminal(&term.id, RunStatus::Exited)
                    .await
                {
                    tracing::warn!(
                        terminal_id = %term.id,
                        error = %e,
                        "failed to complete stale terminal runtime during boot reconcile"
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

pub async fn runtimes_recover_orphans_on_boot(state: &state::AppState) {
    let orphans = match state.repo.runtimes_recover_orphans_on_boot().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "runtime_orphans::recover_on_boot",
                error = %e,
                "runtime orphan scan failed; skipping",
            );
            return;
        }
    };
    if !orphans.is_empty() {
        tracing::warn!(
            target: "runtime_orphans::recover_on_boot",
            count = orphans.len(),
            "runtime orphans detected on boot; no automatic action - see followup",
        );
        for runtime in &orphans {
            tracing::warn!(
                target: "runtime_orphans::recover_on_boot",
                runtime_id = %runtime.id,
                card_id = %runtime.card_id,
                kind = ?runtime.kind,
                status = ?runtime.status,
                "orphan runtime",
            );
        }
    }
}

pub async fn recover_operations_on_boot(state: &state::AppState) -> crate::error::Result<()> {
    let plan = state.operation_runtime.recover_on_boot().await?;
    for item in &plan.items {
        tracing::info!(item = ?item, "operation recovery plan item");
    }
    state.operation_runtime.apply_recovery(plan).await
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
async fn boot_revive_program_for_terminal(
    repo: &dyn db::RouteRepo,
    term: &model::Terminal,
    card: Option<&model::Card>,
    claude_bin: &str,
) -> crate::error::Result<String> {
    let Some(card) = card else {
        return Ok(term.program.clone());
    };
    if card.kind != "claude" {
        return Ok(term.program.clone());
    }
    let payload = &card.payload;
    let Some(claude_session_id) =
        runtime_lookup::resolve_claude_session_for_card(repo, card.id.as_str()).await?
    else {
        return Ok(term.program.clone());
    };
    let Some(settings_path) = payload
        .get("settings_path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(term.program.clone());
    };

    Ok(format!(
        "{} --settings {} --resume {}",
        routes::codex_cards::shell_single_quote(claude_bin),
        routes::codex_cards::shell_single_quote(settings_path),
        routes::codex_cards::shell_single_quote(&claude_session_id),
    ))
}

#[cfg(test)]
mod claude_boot_revive_tests {
    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, runtime_start_tx};
    use crate::model::{NewCard, NewCove, NewWave, new_id, now_ms};
    use crate::runtime_repo::{AgentProvider, RuntimeInit, RuntimeKind};
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

    async fn fresh_repo() -> SqlxRepo {
        SqlxRepo::open("sqlite::memory:").await.unwrap()
    }

    async fn card(repo: &SqlxRepo, kind: &str, payload: serde_json::Value) -> model::Card {
        let cove = repo
            .cove_create(NewCove {
                name: "claude revive".into(),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "claude revive".into(),
                sort: None,
                cwd: "/workspace".into(),
                attach_folder: false,
                theme: routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        repo.card_create(NewCard {
            wave_id: wave.id,
            kind: kind.into(),
            sort: None,
            payload,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn claude_boot_revive_rebuilds_resume_command_from_runtime() {
        let repo = fresh_repo().await;
        let term = terminal(
            "'/opt/claude' --settings '/tmp/settings.json' --session-id '11111111-1111-4111-8111-111111111111' -- 'first prompt'",
            "/workspace",
        );
        let claude = card(
            &repo,
            "claude",
            json!({
                "schemaVersion": 1,
                "terminal_id": "term-1",
                "settings_path": "/tmp/settings.json",
                "cwd": "/workspace",
                "prompt": "first prompt",
                "claude_session_id": "33333333-3333-4333-8333-333333333333"
            }),
        )
        .await;
        let mut tx = repo.pool().begin().await.unwrap();
        runtime_start_tx(
            &mut tx,
            RuntimeInit {
                id: new_id(),
                card_id: claude.id.to_string(),
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                status: RunStatus::Running,
                terminal_run_id: None,
                thread_id: None,
                session_id: Some("22222222-2222-4222-8222-222222222222".into()),
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let program = boot_revive_program_for_terminal(&repo, &term, Some(&claude), "/opt/claude")
            .await
            .unwrap();

        assert_eq!(
            program,
            "'/opt/claude' --settings '/tmp/settings.json' --resume '22222222-2222-4222-8222-222222222222'"
        );
        assert!(!program.contains("--session-id"));
        assert!(!program.contains("--fork-session"));
        assert!(!program.contains("first prompt"));
        assert_eq!(term.cwd, "/workspace");
    }

    #[tokio::test]
    async fn claude_boot_revive_falls_back_to_payload_session_id() {
        let repo = fresh_repo().await;
        let term = terminal(
            "'/opt/claude' --settings '/tmp/settings.json' --session-id '11111111-1111-4111-8111-111111111111' -- 'first prompt'",
            "/workspace",
        );
        let claude = card(
            &repo,
            "claude",
            json!({
                "schemaVersion": 1,
                "terminal_id": "term-1",
                "settings_path": "/tmp/settings.json",
                "cwd": "/workspace",
                "prompt": "first prompt",
                "claude_session_id": "22222222-2222-4222-8222-222222222222"
            }),
        )
        .await;

        let program = boot_revive_program_for_terminal(&repo, &term, Some(&claude), "/opt/claude")
            .await
            .unwrap();

        assert_eq!(
            program,
            "'/opt/claude' --settings '/tmp/settings.json' --resume '22222222-2222-4222-8222-222222222222'"
        );
    }

    #[tokio::test]
    async fn claude_boot_revive_without_session_id_keeps_legacy_fresh_spawn_program() {
        let repo = fresh_repo().await;
        let original = "'/opt/claude' --settings '/tmp/settings.json' -- 'first prompt'";
        let term = terminal(original, "/workspace");
        let legacy = card(
            &repo,
            "claude",
            json!({
                "schemaVersion": 1,
                "terminal_id": "term-1",
                "settings_path": "/tmp/settings.json",
                "cwd": "/workspace",
                "prompt": "first prompt"
            }),
        )
        .await;

        let program = boot_revive_program_for_terminal(&repo, &term, Some(&legacy), "/opt/claude")
            .await
            .unwrap();

        assert_eq!(program, original);
        assert!(!program.contains("--resume"));
    }
}

pub async fn cleanup_legacy_spec_rows_on_boot(state: &state::AppState) {
    let cards = match state.repo.legacy_spec_cards_for_boot_cleanup().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "legacy spec boot cleanup query failed; skipping"
            );
            return;
        }
    };

    for card in cards {
        // R3 P2: before marking the row failed, reap any live legacy
        // app-server process group that survived the upgrade. The legacy
        // takeover path (deleted in PR7c) used to verify the identity
        // stamp + signal the pgid; here we replicate just the reap so an
        // orphaned codex daemon doesn't keep running while the UI shows
        // the spec card as failed. boot-recovery utilities in
        // spec_appserver.rs (verify_owned_pid / signal_process_group)
        // were intentionally kept post-PR7c for this kind of one-shot
        // use.
        let pgid = card
            .payload
            .get("appserver_pgid")
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i32::try_from(v).ok());
        let start_time = card
            .payload
            .get("appserver_start_time")
            .and_then(serde_json::Value::as_u64);
        let boot_id = card
            .payload
            .get("appserver_boot_id")
            .and_then(serde_json::Value::as_str);
        if let (Some(pgid), Some(start_time), Some(boot_id)) = (pgid, start_time, boot_id) {
            if pgid > 1 && spec_appserver::verify_owned_pid(pgid, start_time, boot_id) {
                let sigterm_sent = spec_appserver::signal_process_group(pgid, libc::SIGTERM);
                tracing::warn!(
                    card_id = %card.id,
                    wave_id = %card.wave_id,
                    pgid,
                    sigterm_sent,
                    "legacy spec row had verified live app-server pgid; sent SIGTERM before marking failed"
                );
                if sigterm_sent {
                    tokio::time::sleep(GROUP_KILL_GRACE).await;
                    let sigkill_sent = spec_appserver::signal_process_group(pgid, libc::SIGKILL);
                    tracing::warn!(
                        card_id = %card.id,
                        wave_id = %card.wave_id,
                        pgid,
                        sigkill_sent,
                        "legacy spec row app-server pgid grace elapsed; sent SIGKILL"
                    );
                }
            } else {
                tracing::info!(
                    card_id = %card.id,
                    wave_id = %card.wave_id,
                    ?pgid,
                    "legacy spec row's persisted pgid did not verify (stale or recycled); skipping reap"
                );
            }
        }

        let appserver_root = state
            .daemon
            .data_dir
            .parent()
            .unwrap_or(&state.daemon.data_dir)
            .join("appserver");
        let card_id_str = card.id.to_string();
        if let Some(sock_str) = card
            .payload
            .get("appserver_sock")
            .and_then(serde_json::Value::as_str)
        {
            let sock_stripped = sock_str.strip_prefix("unix://").unwrap_or(sock_str);
            let sock = std::path::Path::new(sock_stripped);
            let is_safe = sock.is_absolute()
                && sock.starts_with(&appserver_root)
                && sock
                    .components()
                    .any(|c| c.as_os_str() == std::ffi::OsStr::new(&card_id_str))
                && std::fs::symlink_metadata(sock)
                    .map(|m| !m.file_type().is_symlink())
                    .unwrap_or(true);

            if is_safe {
                match spec_appserver::cleanup_sock_dir(sock) {
                    spec_appserver::SockDirCleanupOutcome::Removed => tracing::info!(
                        card_id = %card.id,
                        wave_id = %card.wave_id,
                        sock = %sock.display(),
                        outcome = "removed",
                        "legacy spec boot cleanup removed persisted app-server socket"
                    ),
                    spec_appserver::SockDirCleanupOutcome::NotPresent => tracing::info!(
                        card_id = %card.id,
                        wave_id = %card.wave_id,
                        sock = %sock.display(),
                        outcome = "not-present",
                        "legacy spec boot cleanup app-server socket was already absent"
                    ),
                    spec_appserver::SockDirCleanupOutcome::Error(e) => tracing::warn!(
                        card_id = %card.id,
                        wave_id = %card.wave_id,
                        sock = %sock.display(),
                        error = %e,
                        outcome = "error",
                        "legacy spec boot cleanup failed to remove persisted app-server socket"
                    ),
                }
            } else {
                tracing::warn!(
                    card_id = %card.id,
                    wave_id = %card.wave_id,
                    sock = sock_str,
                    "legacy spec row's appserver_sock failed path validation; skipping unlink"
                );
            }
        }

        let scope = match routes::cards::card_scope(
            state.repo.as_ref(),
            card.id.clone(),
            card.wave_id.clone(),
        )
        .await
        {
            Ok(scope) => scope,
            Err(e) => {
                tracing::warn!(
                    card_id = %card.id,
                    wave_id = %card.wave_id,
                    error = %e,
                    "legacy spec boot cleanup failed to resolve card scope; leaving row unchanged"
                );
                continue;
            }
        };

        let card_id = card.id.to_string();
        let wave_id = card.wave_id.to_string();
        let log_card_id = card_id.clone();
        let log_wave_id = wave_id.clone();
        let card_for_event = card;
        let result = db::write_with_event_typed(
            state.repo.as_ref(),
            crate::ids::ActorId::Kernel,
            scope,
            None,
            &state.events,
            state.write(),
            move |tx| {
                Box::pin(async move {
                    if let Some(runtime) =
                        db::sqlite::runtime_get_active_for_card_tx(tx, &card_id).await?
                    {
                        db::sqlite::runtime_complete_tx(
                            tx,
                            &runtime.id,
                            crate::runtime_repo::RunStatus::Failed,
                        )
                        .await?;
                    }
                    let updated = card_for_event;
                    Ok((updated.clone(), crate::event::Event::CardUpdated(updated)))
                })
            },
        )
        .await;

        match result {
            Ok((_card, _event_id)) => {
                let mapping = match state.repo.card_codex_thread_get_by_card(&log_card_id).await {
                    Ok(mapping) => mapping,
                    Err(e) => {
                        tracing::warn!(
                            card_id = %log_card_id,
                            wave_id = %log_wave_id,
                            error = %e,
                            "legacy spec boot cleanup failed to fetch card_codex_threads row before delete"
                        );
                        None
                    }
                };

                match state
                    .repo
                    .card_codex_thread_delete_by_card(&log_card_id)
                    .await
                {
                    Ok(()) => {
                        if let Some(mapping) = mapping {
                            tracing::warn!(
                                card_id = %log_card_id,
                                wave_id = %log_wave_id,
                                thread_id = %mapping.thread_id,
                                "legacy spec boot cleanup deleted stale card_codex_threads row"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            card_id = %log_card_id,
                            wave_id = %log_wave_id,
                            error = %e,
                            "legacy spec boot cleanup failed to delete stale card_codex_threads row"
                        );
                    }
                }
                tracing::warn!(
                    card_id = %log_card_id,
                    wave_id = %log_wave_id,
                    "legacy spec row cannot be taken over by shared daemon; marked active runtime failed when present"
                );
            }
            Err(e) => {
                tracing::warn!(
                    card_id = %log_card_id,
                    wave_id = %log_wave_id,
                    error = %e,
                    "legacy spec boot cleanup failed to mark active runtime failed"
                );
            }
        }
    }
}

/// Mirrors the private terminal/shared-daemon grace window before forcing a
/// process group down after SIGTERM.
const GROUP_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub mod card_fsm;
pub mod card_role_cache;
pub mod codex_appserver;
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
pub mod operation;
pub mod pending_codex_threads;
pub mod plugin_host;
pub(crate) mod proc_supervisor;
pub mod replay;
pub mod role_gate;
pub mod routes;
pub mod runtime_lookup;
pub mod runtime_repo;
pub mod shared_codex_appserver;
pub mod shared_codex_home;
pub mod spec_appserver;
pub mod spec_card;
pub mod spec_push;
pub mod state;
pub mod terminal_renderer;
pub mod terminal_sweeper;
pub mod validation;
pub mod wave_cove_cache;
pub mod wave_lifecycle;
pub mod wave_report;
pub mod wave_report_doc;
pub mod ws;

#[cfg(test)]
mod boot_order_tests {
    #[test]
    fn main_boot_order_harness_supervisor_runtimes_operations() {
        let main_rs = include_str!("main.rs");
        let daemon_start = main_rs
            .find("shared_codex_appserver.start_or_takeover().await")
            .expect("main boot starts shared codex app-server");
        let harness_recover = main_rs
            .find("recover_harnesses_on_boot")
            .expect("main boot recovers spec harnesses");
        let reconcile = main_rs
            .find("reconcile_supervisor_on_boot(&state).await")
            .expect("main boot calls reconcile_supervisor_on_boot");
        let runtimes = main_rs
            .find("runtimes_recover_orphans_on_boot(&state).await")
            .expect("main boot calls runtimes_recover_orphans_on_boot");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        assert!(daemon_start < harness_recover);
        assert!(harness_recover < reconcile);
        assert!(reconcile < runtimes);
        assert!(runtimes < recover);
    }

    #[test]
    fn boot_order_calls_runtime_orphan_recovery_between_supervisor_and_operations() {
        let main_rs = include_str!("main.rs");
        let reconcile = main_rs
            .find("reconcile_supervisor_on_boot(&state).await")
            .expect("main boot calls reconcile_supervisor_on_boot");
        let runtimes = main_rs
            .find("runtimes_recover_orphans_on_boot(&state).await")
            .expect("main boot calls runtimes_recover_orphans_on_boot");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        assert!(reconcile < runtimes);
        assert!(runtimes < recover);
    }
}
