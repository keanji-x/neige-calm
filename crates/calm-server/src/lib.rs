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

pub async fn takeover_shared_spec_cards_on_boot(state: &state::AppState) {
    if !state.shared_codex_appserver.is_running() {
        return;
    }
    // Keep paired with sqlite.rs's OR EXISTS branch: runtime-only empty-thread
    // spec cards must still re-enter pending boot takeover before legacy stamps exist.
    let pending_initial_prompt_cards = match state
        .repo
        .shared_spec_cards_for_initial_prompt_takeover()
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "shared_codex_daemon::spec_card",
                error = %e,
                "shared spec pending boot takeover query failed; skipping"
            );
            Vec::new()
        }
    };
    let active_threads =
        match crate::runtime_lookup::merge_active_shared_thread_attribution(state.repo.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::spec_card",
                    error = %e,
                    "shared spec boot takeover query failed; skipping"
                );
                return;
            }
        };
    let mut resumed = 0usize;
    let mut pending_reparked = 0usize;
    for (card_id, thread_id) in active_threads {
        let card_key: crate::ids::CardId = card_id.clone().into();
        let role = if let Some(role) = state.write().verify_role(&card_key) {
            Some(role)
        } else {
            match state.repo.card_role_get(&card_id).await {
                Ok(role) => role,
                Err(e) => {
                    tracing::warn!(
                        target: "shared_codex_daemon::spec_card",
                        card_id = %card_id,
                        error = %e,
                        "shared spec boot takeover role lookup failed"
                    );
                    continue;
                }
            }
        };
        if role != Some(crate::model::CardRole::Spec) {
            continue;
        }
        let Some(card) = (match state.repo.card_get(&card_id).await {
            Ok(card) => card,
            Err(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::spec_card",
                    card_id = %card_id,
                    error = %e,
                    "shared spec boot takeover card lookup failed"
                );
                continue;
            }
        }) else {
            continue;
        };
        let wave_id = card.wave_id.to_string();
        let Some(wave) = (match state.repo.wave_get(&wave_id).await {
            Ok(wave) => wave,
            Err(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::spec_card",
                    wave_id = %wave_id,
                    error = %e,
                    "shared spec boot takeover wave lookup failed"
                );
                continue;
            }
        }) else {
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
        let watermark = boot_takeover_watermark(&card.payload);
        let wave_key: crate::ids::WaveId = wave_id.clone().into();
        let status: spec_push::SharedStatus =
            std::sync::Arc::new(tokio::sync::Mutex::new(spec_push::SpecPushStatus {
                phase: spec_push::SpecPushPhase::Resumed,
                last_thread_id: Some(thread_id.clone()),
                last_turn_id: None,
            }));
        let mut handle = spec_push::park_shared_handle(
            state.shared_codex_appserver.clone(),
            Some(thread_id.clone()),
            state.shared_codex_appserver.subscribe_notifications(),
            status.clone(),
            None,
            spec_push::TurnWatchdogConfig::default(),
        );
        handle.resume_reconciler = Some(tokio::spawn(spec_push::resume_reconcile_task(
            spec_push::RESUMED_RECONCILE_BUDGET,
            thread_id.clone(),
            status,
            handle.pusher().source,
            handle.queue.clone(),
            handle.watermark_sink.clone(),
            handle.queue_persist.clone(),
        )));
        register_and_catch_up(state, &card_id, &wave_key, watermark, handle, true).await;
        tracing::info!(
            target: "shared_codex_daemon::spec_card",
            card_id = %card_id,
            wave_id = %wave_id,
            thread_id = %thread_id,
            "shared spec boot takeover re-parked handle"
        );
        resumed += 1;
    }
    for (card_id, wave_id, terminal_id, watermark) in pending_initial_prompt_cards {
        if let Err(e) = state
            .pending_codex_threads
            .register(
                crate::pending_codex_threads::PendingEntry::new(
                    card_id.clone(),
                    Some(wave_id.clone()),
                    terminal_id.clone(),
                )
                .with_role(crate::model::CardRole::Spec),
            )
            .await
        {
            tracing::warn!(
                target: "shared_codex_daemon::spec_card",
                card_id,
                wave_id,
                terminal_id,
                error = %e,
                "shared spec pending boot takeover failed to register pending thread"
            );
            continue;
        }
        let wave_key: crate::ids::WaveId = wave_id.clone().into();
        let status: spec_push::SharedStatus =
            std::sync::Arc::new(tokio::sync::Mutex::new(spec_push::SpecPushStatus {
                phase: spec_push::SpecPushPhase::PendingThreadStart,
                last_thread_id: None,
                last_turn_id: None,
            }));
        let handle = spec_push::park_shared_handle(
            state.shared_codex_appserver.clone(),
            None,
            state.shared_codex_appserver.subscribe_notifications(),
            status,
            Some(card_id.clone()),
            spec_push::TurnWatchdogConfig::default(),
        );
        register_and_catch_up(state, &card_id, &wave_key, watermark, handle, true).await;
        tracing::info!(
            target: "shared_codex_daemon::spec_card",
            card_id,
            wave_id,
            terminal_id,
            "shared spec boot takeover re-parked pending handle"
        );
        pending_reparked += 1;
    }
    if resumed > 0 || pending_reparked > 0 {
        tracing::info!(
            target: "shared_codex_daemon::spec_card",
            resumed,
            pending_reparked,
            "shared spec boot takeover complete"
        );
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

fn boot_takeover_watermark(payload: &serde_json::Value) -> i64 {
    payload
        .get("push_watermark")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

async fn register_and_catch_up(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    handle: spec_push::SpecPushHandle,
    reset_cursor_to_watermark: bool,
) {
    let card_key: crate::ids::CardId = card_id.to_string().into();

    // #315 round-4 (B1 defence-in-depth) — the SQL filter in
    // `legacy spec takeover query` is the primary guard against a
    // non-spec card's payload field colliding with the takeover query
    // (see `db/sqlite.rs::legacy spec takeover query` for the
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
            .write()
            .verify_role(&card_key)
            .is_none_or(|role| role == crate::model::CardRole::Spec),
        "register_and_catch_up: card {card_id:?} is not CardRole::Spec; \
         the boot-takeover query MUST scope to spec-role cards \
         (see legacy spec takeover query)"
    );

    // B1 — install the watermark sink on the handle BEFORE the handle is
    // parked in the registry, so the very first queue flush triggered by
    // a catch-up push hitting `Enqueue` has a persister to call.
    //
    // #313 problem #1 round-3 (N7) — `debug_assert!` symmetric with the
    // sister install site in `routes/waves.rs::legacy spec spawner`. A
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
        match state.write().verify_cove(wave_id) {
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
    // Sister install: `routes/waves.rs::legacy spec spawner` (create-wave
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
    // while holding the lock because `tokio::sync::Mutex` is not reentrant.
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
    {
        let guard = state.dispatcher.push_lock(wave_id).await;
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
            &guard,
            state,
            card_id,
            wave_id,
            watermark,
            rehydrated_ids,
        )
        .await;
    }
}

/// Rehydrate durable queue rows and replay event-log rows for a reset-created,
/// already-parked spec push handle. The caller must hold the per-wave push
/// lock; this helper intentionally uses `catch_up_push_under_lock`.
pub(crate) async fn rehydrate_and_catch_up_parked_spec_push_under_lock_parts(
    guard: &dispatcher::PushLockGuard,
    route: &state::RouteState,
    worker: &state::WorkerState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
) {
    let card_key: crate::ids::CardId = card_id.to_string().into();
    let rehydrated_ids = worker
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
    worker
        .dispatcher
        .reset_push_cursor_to_watermark(card_key, watermark);
    replay_spec_push_catch_up_under_lock_parts(
        guard,
        route,
        worker,
        card_id,
        wave_id,
        watermark,
        rehydrated_ids,
    )
    .await;
}

async fn replay_spec_push_catch_up_under_lock(
    guard: &dispatcher::PushLockGuard,
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    rehydrated_ids: Vec<i64>,
) {
    let route: state::RouteState = axum::extract::FromRef::from_ref(state);
    let worker: state::WorkerState = axum::extract::FromRef::from_ref(state);
    replay_spec_push_catch_up_under_lock_parts(
        guard,
        &route,
        &worker,
        card_id,
        wave_id,
        watermark,
        rehydrated_ids,
    )
    .await;
}

async fn replay_spec_push_catch_up_under_lock_parts(
    guard: &dispatcher::PushLockGuard,
    route: &state::RouteState,
    worker: &state::WorkerState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    rehydrated_ids: Vec<i64>,
) {
    let rehydrated_skip: std::collections::HashSet<i64> = rehydrated_ids.iter().copied().collect();
    let rehydrated_count = rehydrated_ids.len();
    let rows = match route.repo.events_since(watermark, None).await {
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
        if !dispatcher::event_warrants_spec_push(&ev, &route.write) {
            continue;
        }
        if rehydrated_skip.contains(&id) {
            skipped_rehydrated += 1;
            continue;
        }
        worker
            .dispatcher
            .catch_up_push_under_lock(guard, ev, id)
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
        && let Some(pusher) = worker.spec_push.pusher(wave_id)
    {
        pusher.flush_pending().await;
    }
}

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
    fn main_boot_order_keeps_operation_recovery_after_supervisor_reconcile() {
        let main_rs = include_str!("main.rs");
        let reconcile = main_rs
            .find("reconcile_supervisor_on_boot(&state).await")
            .expect("main boot calls reconcile_supervisor_on_boot");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        let takeover = main_rs
            .find("takeover_shared_spec_cards_on_boot(&state).await")
            .expect("main boot calls takeover_shared_spec_cards_on_boot");
        assert!(reconcile < recover);
        assert!(recover < takeover);
    }
}
