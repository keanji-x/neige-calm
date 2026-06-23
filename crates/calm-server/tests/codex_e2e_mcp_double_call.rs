//! True E2E reproduction for #569's "MCP stuck in running…" symptom.
//!
//! ## What this test does
//!
//! Boots a real `SharedCodexAppServer` (codex 0.13x daemon) + a real
//! `McpServer` (kernel-as-MCP-server) + the real `neige-mcp-stdio-shim`
//! binary. Starts a Spec card thread, sends ONE turn with an explicit
//! prompt forcing two `calm.task.verdict` calls. Counts
//! `mcpToolCall` `item/started` vs `item/completed` notifications for
//! 90 s. Asserts both calls complete.
//!
//! ## Current status: PASSES, regression net for #569 codex approval gap
//!
//! Native dotted `calm.*` `tools/call` dispatch from a codex worker → the
//! kernel **works on codex 0.137.0** (verified by a real-codex spike for
//! #838; see issue #838 comments). The end-to-end path is:
//!
//!   - kernel MCP server gets `initialize` (daemon trust) ✓
//!   - kernel returns `tools/list` (incl. `calm.task.verdict`) ✓
//!   - LLM returns `function_call name="calm_task_verdict"
//!     namespace="mcp__calm"` ✓
//!   - codex emits `McpToolCallBegin` → `item/started` ✓
//!   - codex sends the `tools/call` to the kernel; the sanitized name
//!     `calm_task_verdict` (ns `mcp__calm`) is correctly reverse-mapped to
//!     the dotted `calm.task.verdict` — there is **no** `mcp__calmcalm_*`
//!     name mangling, and the kernel does no name mangling either
//!     (exact `registry.lookup`) ✓
//!   - kernel runs the handler, returns a result → `McpToolCallEnd` →
//!     `item/completed`. The spike saw `started=2 completed=2`. ✓
//!
//! ## #569 root cause (corrected)
//!
//! The original #569 "stuck in running…" symptom was **missing tool
//! annotations**, not dotted-name mangling. Codex defaults a tool with no
//! annotations to approval-required, which stalls the call before the
//! `tools/call` is ever dispatched. The fix was attaching
//! `role_gated_write_annotations()` to the `calm.*` write tools
//! (`mcp_server/tools/emit.rs`); with annotations present there is no
//! approval stall and the dispatch completes. The earlier hypothesis in
//! this file's history — that codex "never sends `tools/call`" and that
//! the sanitized-name reverse mapping produced `mcp__calmcalm_*` —
//! is **stale and disproven on 0.137.0**.
//!
//! ## Why ship it
//!
//! Deterministic, single-shot, headless regression net: if codex ever
//! regresses native `calm.*` dispatch (or the approval-annotation
//! contract), this test goes RED. The `#[ignore]` gate keeps it out of
//! normal `cargo test` runs (no codex on CI). Operator workflow:
//!
//! ```sh
//! NEIGE_CODEX_BIN=/path/to/codex \
//!   cargo test --features codex-e2e -p calm-server \
//!     --test codex_e2e_mcp_double_call -- --ignored --nocapture
//! ```
//!
//! Post-mortem debug root persists at `/tmp/neige-mcp-double-call-debug`:
//!   - `codex-home/sessions/.../*.jsonl` (codex's turn rollout)
//!   - `codex-home/logs_2.sqlite` (codex's structured `logs` table)
//!   - `logs/shared-codex-appserver/stderr.log` (codex stderr stream)

#![cfg(all(unix, feature = "codex-e2e"))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::{
    ClientInfo, CodexAppServer, InputItem, Notification, ThreadStartParams,
};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, auth, build_default_registry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::routes::theme::RequestTheme;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::WriteContext;
use calm_server::wave_cove_cache::WaveCoveCache;
use clap::Parser;
use serde_json::{Value, json};

const TEST_CWD: &str = "/tmp";

fn codex_bin() -> String {
    std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| "codex".to_string())
}

fn codex_available(codex_bin: &str) -> bool {
    std::process::Command::new(codex_bin)
        .arg("--version")
        .output()
        .is_ok()
}

fn cfg(root: &std::path::Path, codex_bin: &str) -> Config {
    Config::parse_from(vec![
        "calm-server".to_string(),
        "--data-dir".to_string(),
        root.to_str().unwrap().to_string(),
        "--codex-bin".to_string(),
        codex_bin.to_string(),
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

async fn seed_spec_card(repo: &SqlxRepo, card_role_cache: &CardRoleCache) -> (String, String) {
    let cove = repo
        .cove_create(NewCove {
            name: "codex-mcp-double-call".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "double-call-wave".into(),
            sort: None,
            cwd: TEST_CWD.into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
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
                "codex_source": "shared",
                "spec_harness": true
            }),
        },
        CardRole::Spec,
        false,
        card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    (card_id, wave.id.to_string())
}

async fn seed_shared_spec_runtime(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card_id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
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

fn item_type(params: &Value) -> Option<&str> {
    params
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
}

fn item_id(params: &Value) -> Option<&str> {
    params
        .get("item")
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
}

#[tokio::test]
#[ignore]
async fn codex_mcp_double_call_both_complete() {
    let codex_bin = codex_bin();
    if !codex_available(&codex_bin) {
        eprintln!("skipping: codex not on PATH and NEIGE_CODEX_BIN not usable");
        return;
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();

    // Pin a known root so codex's session + daemon logs survive panic for
    // post-mortem. Hardcoded path lets the operator just `tail` it.
    let root_path = std::path::PathBuf::from("/tmp/neige-mcp-double-call-debug");
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(&root_path).expect("mkdir debug root");
    eprintln!(
        "[double-call] DEBUG root persisted at {} — codex-home/sessions, app-server-daemon/app-server.stderr.log live here",
        root_path.display(),
    );
    struct Root(std::path::PathBuf);
    impl AsRef<std::path::Path> for Root {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }
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
    let (card_id, wave_id) = seed_spec_card(&repo, &card_role_cache).await;
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    eprintln!("[double-call] seeded wave={wave_id} spec_card={card_id}");

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
        events,
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
    eprintln!(
        "[double-call] mcp socket={} shim={}",
        mcp_socket_path.display(),
        mcp_server.shim_config.shim_bin.display()
    );

    let cfg = cfg(root.path(), &codex_bin);
    let home = Arc::new(SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    ));
    seed_auth_only(&home);
    home.ensure_config_for_cwd(Path::new(TEST_CWD))
        .expect("trust test cwd in shared CODEX_HOME");
    home.ensure_daemon_mcp_config(&mcp_server.shim_config, &daemon_token)
        .expect("write shared daemon MCP config");

    let daemon = SharedCodexAppServer::new(&cfg, home, repo_dyn.clone());
    if let Err(e) = daemon.start_or_takeover().await {
        eprintln!("skipping: shared app-server did not boot in this environment: {e}");
        return;
    }
    eprintln!("[double-call] shared codex daemon started");

    let thread_config = json!({
        "shell_environment_policy": {
            "set": {
                "NEIGE_MCP_SOCKET": mcp_socket_path.to_string_lossy().to_string(),
                "NEIGE_MCP_DAEMON_TOKEN": daemon_token,
            }
        }
    });
    let remote = daemon.remote_uri();
    let socket_path = remote
        .strip_prefix("unix://")
        .expect("shared daemon remote URI must be unix://");
    let (wire_client, _wire_notifications) = CodexAppServer::connect(socket_path)
        .await
        .expect("connect wire test client");
    wire_client
        .initialize(ClientInfo {
            name: "neige-calm-double-call-wire-test".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await
        .expect("initialize wire test client");
    let thread = wire_client
        .thread_start_with_params(ThreadStartParams {
            cwd: TEST_CWD.into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: Some(thread_config),
        })
        .await
        .expect("wire thread_start_with_params");
    let thread_id = thread
        .thread_id()
        .expect("thread/start returned no thread.id")
        .to_string();
    seed_shared_spec_runtime(&repo, &card_id, &thread_id).await;
    eprintln!("[double-call] thread_id={thread_id} runtime_attribution=seeded");

    let mut rx = daemon.subscribe_notifications();
    let prompt = "You have one MCP tool available: calm.task.verdict. \
You MUST call it exactly twice, in sequence, with these exact arguments. \
Do NOT output any text before, between, or after the calls. \
First call: { \"idempotency_key\": \"double-call-first\", \"status\": \"accepted\", \"reason\": \"first probe\" } \
Second call: { \"idempotency_key\": \"double-call-second\", \"status\": \"accepted\", \"reason\": \"second probe\" } \
After the second call returns, output the single word OK and stop.";
    let turn_id = daemon
        .turn_start(&thread_id, vec![InputItem::text(prompt)])
        .await
        .expect("turn_start");
    eprintln!("[double-call] turn_id={turn_id}");

    let mut started = 0_u32;
    let mut completed = 0_u32;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && completed < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(Notification::Item { method, params })) => {
                let notif_thread = params.get("threadId").and_then(Value::as_str);
                if matches!(notif_thread, Some(t) if t != thread_id) {
                    continue;
                }
                if item_type(&params) == Some("mcpToolCall") {
                    match method.as_str() {
                        "item/started" => {
                            started += 1;
                            eprintln!(
                                "[double-call] mcpToolCall started count={started} item_id={:?}",
                                item_id(&params)
                            );
                        }
                        "item/completed" => {
                            completed += 1;
                            eprintln!(
                                "[double-call] mcpToolCall completed count={completed} item_id={:?}",
                                item_id(&params)
                            );
                        }
                        _ => {}
                    }
                }
            }
            Ok(Ok(Notification::TurnCompleted { thread_id: t, turn })) if t == thread_id => {
                eprintln!("[double-call] turn/completed turn={turn}");
                if completed >= 2 {
                    break;
                }
            }
            Ok(Ok(Notification::Other { method, params })) => {
                if method.contains("error") {
                    eprintln!("[double-call] other method={method} params={params}");
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                eprintln!("[double-call] notification receiver lagged by {n}");
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                eprintln!("[double-call] notification channel closed");
                break;
            }
            Err(_) => {
                eprintln!("[double-call] timed out waiting for notifications");
                break;
            }
        }
    }

    let final_title = repo
        .wave_get(&wave_id)
        .await
        .unwrap()
        .map(|wave| wave.title);
    eprintln!("[double-call] started={started} completed={completed}");
    eprintln!("[double-call] final_wave_title={final_title:?}");
    assert_eq!(
        completed, started,
        "every started mcpToolCall must complete: started={started} completed={completed}"
    );
    assert!(
        started >= 2,
        "codex never initiated 2 mcpToolCall: started={started}"
    );
}
