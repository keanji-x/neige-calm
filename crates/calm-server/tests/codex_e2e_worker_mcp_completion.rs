//! A1 acceptance E2E for #838 Move 2 — codex worker completion decouples
//! from channel 3.
//!
//! ## What this test proves
//!
//! A worker agent's only obligation to the kernel is to report completion
//! (`Event::TaskCompleted`). Today that report is mandated through the
//! `neige task-completed` shell CLI, which can only work if the worker's AI
//! exec-shell carries `NEIGE_MCP_SOCKET` + `NEIGE_MCP_TOKEN` (channel 3 —
//! the per-thread `shell_environment_policy.set` block on `thread/start`).
//! That channel keeps getting silently dropped (#738/#747/#836), and when
//! it is, the worker can never report and the wave wedges silently.
//!
//! Move 2 routes **codex** worker completion through the native
//! `calm.task.complete` MCP tool (channel 2 — DaemonTrust + codex-injected
//! `_meta.threadId`), which a shared-daemon worker has regardless of
//! channel 3.
//!
//! What A1 proves is exactly that **channel-2 completion works end-to-end
//! with channel 3 stripped** — the decoupling itself, against the real codex
//! binary. It does NOT guard the prompt swap. The PROMPT mandate (the codex
//! worker uses `calm.task.complete`, not the `neige task-completed` CLI) is
//! locked deterministically elsewhere: the const test
//! `spec_card.rs::worker_codex_prompt_reports_completion_via_mcp_tools_not_cli`
//! pins the rendered prompt text, and the `codex_worker_shared_daemon.rs`
//! `thread/start` contract test pins what the worker spawn actually wires.
//!
//! ## How channel 3 is stripped
//!
//! The worker `thread/start` is issued with `config: None` — i.e. NO
//! `shell_environment_policy` at all. The shared daemon's home-wide MCP
//! config (`ensure_daemon_mcp_config`, channel 2) is still wired, so a
//! native `calm.task.complete` `tools/call` authenticates via the daemon
//! token + `_meta.threadId`, but `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN` are
//! ABSENT from the worker's exec-shell, so the `neige` CLI cannot reach the
//! kernel. (Belt-and-suspenders: the spawn env handed to the daemon also
//! strips both keys.) This is the faithful, minimal reproduction of the
//! "channel 3 dropped" failure mode at the worker exec-shell.
//!
//! ## RED / GREEN
//!
//!   * **GREEN** (shipped, `WORKER_CODEX = true` → `WorkerCodex`,
//!     `calm.task.complete`): the worker reports completion via the native
//!     MCP tool over channel 2 and the kernel commits `TaskCompleted`.
//!     Verified against real codex 0.137.0 (committed in ~12 s).
//!   * **RED** (CLI baseline): flip `WORKER_CODEX = false` → `Worker`
//!     (`neige task-completed`). The CLI needs channel 3, which is stripped,
//!     so the completion never reaches the kernel and the test times out.
//!
//! ### Capturing RED faithfully (a finding worth recording)
//!
//! The visibility flip in Part C (`emit.rs`: `task_complete`/`task_fail`
//! → `visible_to_roles: &[CardRole::Worker]`) makes `calm.task.complete`
//! appear in the worker's `tools/list` regardless of prompt. So a *CLI*-
//! prompted worker, after its `neige task-completed` shell call FAILS
//! (channel 3 stripped — confirmed exit-1 in the rollout), will *fall back*
//! to the now-visible MCP tool and still complete. That is a Move-2
//! robustness win, but it means `WORKER_CODEX = false` alone no longer
//! reliably yields RED once Part C is applied. To capture the *clean*
//! pre-#838 RED (CLI is the worker's ONLY completion path), set
//! `WORKER_CODEX = false` **and** revert Part C (both descriptors back to
//! `visible_to_roles: &[]`) — then the worker times out with no commit.
//! Reverting BOTH the prompt (the `Worker`/CLI mandate) and the visibility
//! flip is not a confound: both were the actual pre-#838 baseline, so this
//! is the faithful repro of the world before Move 2, not an artificial
//! handicap. That exact RED was captured during development; see the PR
//! report.
//!
//! ## Run
//!
//! ```sh
//! NEIGE_CODEX_BIN=/home/kenji/.local/bin/codex \
//! NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
//!   cargo test -p calm-server --features codex-e2e \
//!     --test codex_e2e_worker_mcp_completion -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`-gated + `feature = "codex-e2e"` so CI (no codex) never runs
//! it; it is proven locally against the real codex binary.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::InputItem;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::{Event, EventBus};
use calm_server::mcp_server::{McpServer, auth, build_default_registry};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewWave, WaveLifecycle, WavePatch, new_id, now_ms,
};
use calm_server::routes::theme::RequestTheme;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::{
    SharedCodexAppServer, SharedThreadStartParams, ThreadConfig,
};
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::WriteContext;
use calm_server::wave_cove_cache::WaveCoveCache;
use clap::Parser;
use serde_json::json;
// #868: shared no-fallback resolver — env `NEIGE_CODEX_BIN` only, `None` ⇒
// self-skip via `skip!`. Tests must never fall back to a PATH codex.
use support::codex_fixture::resolve_codex_bin;
use tokio::time::timeout;

const TEST_CWD: &str = "/tmp";

/// Provider toggle. `false` = CLI worker prompt (`neige task-completed`,
/// needs channel 3 → RED baseline). `true` = codex MCP prompt
/// (`calm.task.complete`, rides channel 2 → GREEN, what codex_adapter
/// ships). The shipped value is `true`; flip to `false` to re-capture RED.
const WORKER_CODEX: bool = true;

fn cfg(root: &std::path::Path, codex_bin: &str) -> Config {
    Config::parse_from(vec![
        "calm-server".to_string(),
        "--data-dir".to_string(),
        root.to_str().unwrap().to_string(),
        "--codex-bin".to_string(),
        codex_bin.to_string(),
        // Test codex daemons must NEVER post hooks to the default listen address —
        // that is the production calm-server port on shared boxes (production-kill
        // incident, 2026-07-04); tests do not consume hook ingest.
        "--codex-ingest-url".to_string(),
        "http://127.0.0.1:1/hooks-disabled-in-e2e".to_string(),
    ])
}

fn seed_auth_only(home: &SharedCodexHome) {
    home.seed_from(None).expect("seed empty shared CODEX_HOME");
    let Some(host_home) = std::env::var_os("HOME") else {
        return;
    };
    let src = Path::new(&host_home).join(".codex").join("auth.json");
    if !src.exists() {
        return;
    }
    let dst = home.path().join("auth.json");
    std::fs::copy(&src, &dst).expect("copy host codex auth.json into test CODEX_HOME");
}

async fn seed_worker_card(repo: &SqlxRepo, card_role_cache: &CardRoleCache) -> (String, String) {
    let cove = repo
        .cove_create(NewCove {
            name: "codex-worker-mcp-completion".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "worker-mcp-completion-wave".into(),
            sort: None,
            cwd: TEST_CWD.into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Put the wave in Working so the worker's first report is a legal,
    // unsurprising transition (TaskCompleted auto-promotes Working ->
    // Reviewing). Not strictly required for the assertion, which only
    // waits on TaskCompleted, but keeps the wave shape realistic.
    repo.wave_update(
        wave.id.as_str(),
        WavePatch {
            lifecycle: Some(WaveLifecycle::Working),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let card_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        card_id.clone(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "codex_source": "shared"
            }),
        },
        CardRole::Worker,
        false,
        card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    (card_id, wave.id.to_string())
}

async fn seed_shared_worker_runtime(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card_id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn worker_completes_with_channel3_stripped() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    let codex_bin = codex_bin.display().to_string();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();

    // Pin a known root so codex's session + daemon logs survive panic.
    let root_path = std::path::PathBuf::from("/tmp/neige-worker-mcp-completion-debug");
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(&root_path).expect("mkdir debug root");
    eprintln!(
        "[worker-mcp] DEBUG root persisted at {}",
        root_path.display(),
    );
    struct Root(std::path::PathBuf);
    impl Root {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    let root = Root(root_path);

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let events = EventBus::new();
    // Subscribe BEFORE driving the turn so we never miss the commit.
    let mut bus_rx = events.subscribe();
    let (card_id, wave_id) = seed_worker_card(&repo, &card_role_cache).await;
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    eprintln!("[worker-mcp] seeded wave={wave_id} worker_card={card_id} codex={WORKER_CODEX}");

    let daemon_token = auth::CardMcpToken::generate().into_inner();
    let daemon_token_hash = auth::hash_token(&daemon_token);
    let mcp_socket_path = root.path().join("mcp").join("kernel.sock");
    let shim_bin = {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        p.pop();
        p.push("neige-mcp-stdio-shim");
        assert!(
            p.exists(),
            "neige-mcp-stdio-shim not found at {p:?}; run cargo test with fixtures enabled"
        );
        p
    };
    let mcp_server = McpServer::spawn(
        repo_dyn.clone(),
        events.clone(),
        WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        mcp_socket_path.clone(),
        shim_bin,
        build_default_registry(),
        Some(daemon_token_hash),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("neige-test-gate-logs"),
    )
    .await
    .expect("boot real McpServer");
    eprintln!("[worker-mcp] mcp socket={}", mcp_socket_path.display());

    let cfg = cfg(root.path(), &codex_bin);
    let home = Arc::new(SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    ));
    seed_auth_only(&home);
    home.ensure_config_for_cwd(Path::new(TEST_CWD))
        .expect("trust test cwd in shared CODEX_HOME");
    // Channel 2 ONLY — home-wide daemon MCP config for every codex thread.
    home.ensure_daemon_mcp_config(&mcp_server.shim_config, &daemon_token)
        .expect("write shared daemon MCP config");

    let daemon = SharedCodexAppServer::new(&cfg, home, repo_dyn.clone());
    if let Err(e) = daemon.start_or_takeover().await {
        eprintln!("skipping: shared app-server did not boot in this environment: {e}");
        return;
    }
    eprintln!("[worker-mcp] shared codex daemon started");

    // Render the worker prompt for the role under test. Substitute a
    // concrete idempotency key K the worker is told to echo.
    let idempotency_key = "wm-strip-c3";
    let worker_instructions =
        calm_server::spec_card::render_worker_prompt_for_e2e(&wave_id, WORKER_CODEX);

    // Channel 3 STRIPPED: config is None — NO shell_environment_policy at
    // all, so NEIGE_MCP_SOCKET / NEIGE_MCP_TOKEN never reach the worker's
    // AI exec-shell. The `neige` CLI therefore cannot reach the kernel;
    // only the native MCP tool (channel 2) can.
    let thread_id = daemon
        .thread_start_for_card(
            &card_id,
            CardRole::Worker,
            Some(&wave_id),
            SharedThreadStartParams {
                cwd: TEST_CWD.into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: Some(worker_instructions),
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .expect("thread_start_for_card (channel 3 stripped)");
    seed_shared_worker_runtime(&repo, &card_id, &thread_id).await;
    eprintln!("[worker-mcp] thread_id={thread_id} channel3=STRIPPED runtime=seeded");

    // Hand the worker a trivial task + its idempotency key K. The prompt is
    // provider-neutral on the reporting MECHANISM — the worker follows its
    // own contract (CLI for `Worker`, MCP tool for `WorkerCodex`). The only
    // obligation is the completion report. (Reads are not exercised; they
    // would need channel 3.)
    let prompt = format!(
        "Your task: there is nothing to build — the work is already done. \
Your idempotency key K is \"{idempotency_key}\". Following your reporting \
contract, report task completion exactly once now, then stop."
    );
    let turn_id = daemon
        .turn_start(&thread_id, vec![InputItem::text(&prompt)])
        .await
        .expect("turn_start");
    eprintln!("[worker-mcp] turn_id={turn_id}");

    // Wait for the kernel to commit Event::TaskCompleted for our idempotency
    // key. This is the deterministic worker-completion path: it fires only if
    // the worker's report reached the kernel. Deadline is env-overridable
    // (NEIGE_E2E_DEADLINE_SECS) so the RED baseline can use a shorter window.
    let deadline_secs = std::env::var("NEIGE_E2E_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(270);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    let mut saw_completed = false;
    let mut saw_failed: Option<String> = None;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, bus_rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::TaskCompleted {
                    idempotency_key: k, ..
                } if k == idempotency_key => {
                    eprintln!(
                        "[worker-mcp] TaskCompleted committed key={k} actor={:?}",
                        env.actor
                    );
                    saw_completed = true;
                    break;
                }
                Event::TaskFailed {
                    idempotency_key: k,
                    reason,
                    ..
                } if k == idempotency_key => {
                    eprintln!("[worker-mcp] TaskFailed committed key={k} reason={reason}");
                    saw_failed = Some(reason.clone());
                    break;
                }
                other => {
                    eprintln!("[worker-mcp] other event kind={}", other.kind_tag());
                }
            },
            Ok(Err(e)) => {
                eprintln!("[worker-mcp] bus recv error: {e:?}");
            }
            Err(_) => {
                eprintln!("[worker-mcp] timed out waiting for TaskCompleted");
                break;
            }
        }
    }

    assert!(
        saw_completed,
        "kernel must commit Event::TaskCompleted for key={idempotency_key} \
via the native MCP tool with channel 3 stripped (saw_failed={saw_failed:?}). \
With SeededCardRole::Worker (CLI) this is RED; with WorkerCodex (MCP) GREEN."
    );
    let _ = (&mcp_server, &daemon);
}
