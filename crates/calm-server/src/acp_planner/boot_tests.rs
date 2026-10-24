//! Boot degradation must not weaken credential revocation or the launch fence.
//! Compiled as an ACP owner unit-test module; Rust excludes this file from production.
#![cfg(test)]
#[cfg(target_os = "macos")]
use super::config::{AcpPlannerHost, boot};
use crate::config::Config;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::state::AppState;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use clap::Parser as _;
use serde_json::json;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::Arc;

/// Re-exec just this test with private HOME/seed/temp paths. No process-wide environment edits,
/// host credentials, real agents, or Linux-only orphan cleanup, even under ordinary cargo test.
fn isolated(name: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("ACP_BOOT_TEST_ROOT") {
        return Some(root.into());
    }
    let root = tempfile::Builder::new()
        .prefix("ab-")
        .tempdir_in("/tmp")
        .unwrap();
    for dir in ["home", "seed", "tmp", "xdg"] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            &format!("acp_planner::boot_tests::{name}"),
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path().join("home"))
        .env("CODEX_HOME", root.path().join("seed"))
        .env("TMPDIR", root.path().join("tmp"))
        .env("XDG_CONFIG_HOME", root.path().join("xdg"))
        .env("XDG_DATA_HOME", root.path().join("xdg"))
        .env("XDG_RUNTIME_DIR", root.path().join("xdg"))
        .env("ACP_BOOT_TEST_ROOT", root.path())
        .current_dir(root.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    let status = output.status;
    eprintln!("isolated {name}: child {pid} exited and reaped ({status})");
    assert!(
        stdout.contains("running 1 test"),
        "child must select one test"
    );
    assert!(status.success(), "isolated {name}: {status}");
    None
}

fn config(root: &Path, configured: bool) -> Config {
    std::fs::create_dir_all(root).unwrap();
    let mut cfg = Config::parse_from(["calm-server"]);
    cfg.db_url = format!("sqlite://{}?mode=rwc", root.join("db").display());
    cfg.listen = "127.0.0.1:0".into();
    cfg.data_dir = Some(root.join("data"));
    cfg.workspace_root = Some(root.join("ws"));
    cfg.plugins_dir = Some(root.join("plugins"));
    cfg.plugins_data_dir = Some(root.join("plugin-data"));
    cfg.proc_supervisor_sock = Some(root.join("sup.sock"));
    cfg.codex_bin = root.join("no-codex").display().to_string();
    cfg.claude_bin = root.join("no-claude").display().to_string();
    cfg.codex_bridge_bin = Some(root.join("no-bridge"));
    cfg.mcp_stdio_shim_bin = Some(root.join("no-shim"));
    cfg.auth_dev_autologin = true;
    if configured {
        // Record entry into the executable, even if negotiation would subsequently fail.
        let script = root.join("agent.sh");
        std::fs::write(&script, "printf spawned >> \"$ACP_SPAWN_LOG\"\nexit 1\n").unwrap();
        let path = root.join("acp.json");
        std::fs::write(
            &path,
            json!({"agents":[{
                "provider":"opencode", "command":"/bin/sh", "args":[script],
                "env":{"ACP_SPAWN_LOG":root.join("spawned")},
                "min_agent_version":"1.0.0"
            }]})
            .to_string(),
        )
        .unwrap();
        cfg.acp_planner_config = Some(path);
    }
    cfg
}

async fn http_available(state: &AppState) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = crate::routes::router().with_state(state.clone());
    let serving = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    use http_body_util::BodyExt as _;
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut client, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    let response = client
        .send_request(
            axum::http::Request::builder()
                .uri("/api/track-templates")
                .header("Host", addr.to_string())
                .header("Connection", "close")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    connection.await.unwrap().unwrap();
    serving.abort();
    let _ = serving.await;
    let released = tokio::net::TcpListener::bind(addr).await.unwrap();
    drop(released);
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&body)
            .unwrap()
            .is_array()
    );
    eprintln!("HTTP {addr} served and rebound after shutdown");
}

#[cfg(target_os = "macos")]
fn host(state: &AppState) -> Arc<AcpPlannerHost> {
    use axum::extract::FromRef;
    crate::state::RouteState::from_ref(state).acp_planner
}

async fn close(state: AppState) {
    state
        .mcp_server
        .as_ref()
        .unwrap()
        .stop_listener_for_test()
        .await;
}

#[tokio::test]
async fn acp_boot_unconfigured_serves_http() {
    let Some(root) = isolated("acp_boot_unconfigured_serves_http") else {
        return;
    };
    let state = AppState::boot(&config(&root, false))
        .await
        .expect("unconfigured ACP must not block boot")
        .into_parts()
        .0;
    http_available(&state).await;
    close(state).await;
}

#[tokio::test]
async fn acp_boot_configured_serves_http() {
    let Some(root) = isolated("acp_boot_configured_serves_http") else {
        return;
    };
    let state = AppState::boot(&config(&root, true))
        .await
        .expect("ACP cleanup failure must not block boot")
        .into_parts()
        .0;
    http_available(&state).await;
    close(state).await;
    // Full production boot now includes provider availability probes. Linux can
    // run this configured fake; macOS cleanup failure must still prevent it.
    #[cfg(target_os = "macos")]
    assert!(!root.join("spawned").exists());
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Owner {
    worker: String,
    card: String,
    track: String,
    thread: String,
}

/// The same repository constructors used by the existing Planner session fixture, without its
/// Linux process-scanning Root/Drop. Each row owns its own card and is initially unbound.
async fn owner(repo: &SqlxRepo, root: &Path, provider: AgentProvider) -> Owner {
    owner_with_plan(
        repo,
        root,
        provider,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
    )
    .await
}

async fn owner_with_plan(
    repo: &SqlxRepo,
    root: &Path,
    provider: AgentProvider,
    plan: &crate::db::sqlite::TrackWorkspacePlan,
) -> Owner {
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "ACP boot".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let track = crate::db::sqlite::track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id,
            title: "ACP boot".into(),
            sort: None,
            cwd: root.display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
        None,
        plan,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(&mut tx, new_id(), NewCard {
        track_id: track.id.clone(), title: None, kind: "codex".into(), sort: None,
        payload: json!({"schemaVersion":1,"planner_harness":true,"planner_provider":provider,"permission_mode":"never"}),
    }, CardRole::Planner, false, &crate::card_role_cache::CardRoleCache::new()).await.unwrap();
    let worker = new_id();
    let thread = new_id();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(provider),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Owner {
        worker,
        card: card.id.to_string(),
        track: track.id.to_string(),
        thread,
    }
}

async fn historical(repo: &SqlxRepo, root: &Path, provider: AgentProvider, owned: bool) -> Owner {
    let row = owner(repo, root, provider).await;
    historical_row(repo, row, owned).await
}

async fn historical_row(repo: &SqlxRepo, row: Owner, owned: bool) -> Owner {
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
        .bind(old_hash(&row))
        .bind(&row.worker)
        .execute(repo.pool())
        .await
        .unwrap();
    if owned {
        crate::db::sqlite::acp_registration_claim(repo.db(), &row.worker, "historical", false)
            .await
            .unwrap();
    }
    row
}

fn old_hash(row: &Owner) -> String {
    crate::mcp_server::auth::hash_token(&format!("old-{}", row.worker))
}

async fn authenticates(repo: &SqlxRepo, row: &Owner) -> bool {
    crate::mcp_server::handshake::handle_initialize(
        repo,
        None,
        &json!({"_meta":{"dev.neige/auth":{"token":format!("old-{}", row.worker)}}}),
        crate::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION,
    )
    .await
    .is_ok()
}

async fn token(repo: &SqlxRepo, row: &Owner) -> Option<String> {
    sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
        .bind(&row.worker)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn acp_boot_unconfigured_revokes_only_owned_credentials() {
    let Some(root) = isolated("acp_boot_unconfigured_revokes_only_owned_credentials") else {
        return;
    };
    let cfg = config(&root.join("a"), false);
    let repo = SqlxRepo::open(&cfg.db_url).await.unwrap();
    let owned = historical(&repo, &root, AgentProvider::OpenCode, true).await;
    let retired = historical(&repo, &root, AgentProvider::OpenCode, true).await;
    sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id=?1")
        .bind(&retired.worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let unowned = historical(&repo, &root, AgentProvider::OpenCode, false).await;
    let codex = historical(&repo, &root, AgentProvider::Codex, false).await;
    let other_cfg = config(&root.join("b"), false);
    let other_repo = SqlxRepo::open(&other_cfg.db_url).await.unwrap();
    let other = historical(&other_repo, &root, AgentProvider::OpenCode, true).await;
    for row in [&owned, &unowned, &codex] {
        assert!(
            authenticates(&repo, row).await,
            "old credential must work before boot"
        );
    }
    assert!(authenticates(&other_repo, &other).await);
    let state = AppState::boot(&cfg)
        .await
        .expect("historical ACP rows without configuration must boot")
        .into_parts()
        .0;
    assert!(
        !authenticates(&repo, &owned).await,
        "revoked credential must fail the real MCP handshake"
    );
    assert_eq!(
        token(&repo, &retired).await,
        None,
        "inactive owned rows are revoked too"
    );
    assert_eq!(
        token(&repo, &owned).await,
        None,
        "old owned credential must be revoked before serving"
    );
    for row in [&unowned, &codex] {
        assert!(authenticates(&repo, row).await);
        assert_eq!(token(&repo, row).await, Some(old_hash(row)));
    }
    assert!(authenticates(&other_repo, &other).await);
    assert_eq!(token(&other_repo, &other).await, Some(old_hash(&other)));
    http_available(&state).await;
    close(state).await;
}

#[tokio::test]
async fn acp_boot_revocation_error_prevents_listener() {
    let Some(root) = isolated("acp_boot_revocation_error_prevents_listener") else {
        return;
    };
    let cfg = config(&root, false);
    let repo = SqlxRepo::open(&cfg.db_url).await.unwrap();
    let owned = historical(&repo, &root, AgentProvider::OpenCode, true).await;
    sqlx::query(
        "CREATE TRIGGER refuse_acp_revoke
         BEFORE UPDATE OF mcp_token_hash ON worker_sessions
         WHEN OLD.id IN (SELECT worker_session_id FROM acp_managed_sessions)
         BEGIN SELECT RAISE(ABORT, 'injected ACP revoke failure'); END",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    let error = match AppState::boot(&cfg).await {
        Ok(state) => {
            close(state.into_parts().0).await;
            panic!("revocation failure must abort boot before listening")
        }
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("injected ACP revoke failure"),
        "{error:#}"
    );
    assert_eq!(token(&repo, &owned).await, Some(old_hash(&owned)));
    let socket = crate::mcp_server::transport::default_socket_path(&cfg.data_dir_resolved());
    assert!(!socket.exists(), "MCP listener must never be created");
    assert!(tokio::net::UnixStream::connect(socket).await.is_err());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn acp_boot_turn_refuses_unavailable_executor_before_mint() {
    let Some(root) = isolated("acp_boot_turn_refuses_unavailable_executor_before_mint") else {
        return;
    };
    let cfg = config(&root, true);
    let state = AppState::boot(&cfg).await.unwrap().into_parts().0;
    let repo = Arc::new(SqlxRepo::open(&cfg.db_url).await.unwrap());
    let row = owner(&repo, &root, AgentProvider::OpenCode).await;
    // Managed issuance refuses an unavailable Engine before registration or mint.
    let session = super::session::AcpPlannerSession::open(super::session::SessionParams {
        engine: host(&state).execution.engine().cloned(),
        host: host(&state),
        provider: AgentProvider::OpenCode,
        repo: repo.clone(),
        seals: state.shared_codex_appserver.thread_seals().clone(),
        worker_session_id: row.worker.clone(),
        card_id: row.card.clone(),
        track_id: row.track.clone(),
        cwd: root.clone(),
        instructions: String::new(),
    });
    let (held, _receiver) = crate::harness::held_requests::channel();
    session.mark_installed(held);
    let result = session
        .start_turn(
            &row.thread,
            provider::control::TurnRequest {
                input: provider::control::InputRequest {
                    items: vec![provider::InputItem::text("test")],
                    correlation: provider::control::CorrelationKey(new_id()),
                },
                selection: provider::TurnModelSelection::inherit(),
                permission: crate::planner_permission_mode::PlannerPermissionMode::Never,
                edit: provider::control::TurnEdit::NoEdit,
            },
            &[],
        )
        .await;
    close(state).await;
    assert!(
        !root.join("spawned").exists(),
        "turn must not execute the agent"
    );
    assert!(matches!(
        result,
        Err(crate::harness::backend::TurnStartFailure::Transient(
            crate::error::CalmError::ExecutionUnavailable
        ))
    ));
    assert_eq!(
        token(&repo, &row).await,
        None,
        "an unavailable executor must not mint a credential"
    );
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM acp_managed_sessions WHERE worker_session_id=?1)",
    )
    .bind(&row.worker)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(
        !owned,
        "availability is checked before managed registration"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn acp_boot_track_sweep_without_execution_revokes_without_native_scan() {
    let Some(root) = isolated("acp_boot_track_sweep_without_execution_revokes_without_native_scan")
    else {
        return;
    };
    let cfg = config(&root, false);
    let repo = SqlxRepo::open(&cfg.db_url).await.unwrap();
    let row = historical(&repo, &root, AgentProvider::OpenCode, true).await;
    std::fs::create_dir_all(cfg.data_dir_resolved()).unwrap();
    let host = AcpPlannerHost::new(
        None,
        &cfg.data_dir_resolved(),
        root.join("no-shim"),
        root.join("mcp.sock"),
    )
    .unwrap();
    super::config::boot_with_sweep(&repo, &host, |_| async { Ok(()) })
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
        .bind(old_hash(&row))
        .bind(&row.worker)
        .execute(repo.pool())
        .await
        .unwrap();
    // The pre-C4 native scan remains boot-only.
    super::config::sweep_track(&repo, &host, &row.track)
        .await
        .unwrap();
    assert_eq!(token(&repo, &row).await, None);
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod authentication_tests;
#[cfg(all(target_os = "linux", feature = "fixtures"))]
#[cfg(test)]
mod receipt_tests;

#[tokio::test]
async fn acp_pre_c4_failed_boot_fences_each_strict_stop() {
    use super::config::AcpPlannerHost;
    use crate::harness::backend::factory::PlannerFactory;
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path(), false);
    let repo = Arc::new(SqlxRepo::open(&cfg.db_url).await.unwrap());
    let row = historical(&repo, root.path(), AgentProvider::OpenCode, true).await;
    std::fs::create_dir_all(cfg.data_dir_resolved()).unwrap();
    let host = Arc::new(
        AcpPlannerHost::new(
            None,
            &cfg.data_dir_resolved(),
            "no-shim".into(),
            root.path().join("mcp.sock"),
        )
        .unwrap(),
    );
    host.completion.bind(repo.clone());
    let expected = row.worker.clone();
    super::config::boot_with_sweep(repo.as_ref(), &host, |markers| async move {
        assert!(markers.contains(&expected));
        Err(crate::error::CalmError::Conflict(
            "injected pre-C4 sweep failure".into(),
        ))
    })
    .await
    .unwrap();
    assert_eq!(token(&repo, &row).await, None);
    let claude =
        Arc::new(crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap());
    let factory = PlannerFactory::new(std::sync::Weak::new(), &claude, &host);
    let runtime = repo
        .session_projection_by_id(&row.worker)
        .await
        .unwrap()
        .unwrap();
    let registry = crate::harness::HarnessRegistry::new();
    let results = [
        factory.shutdown_replay(&runtime).await,
        factory
            .stop_historical_predecessor(&AgentProvider::OpenCode, &row.card, &row.worker)
            .await,
        factory
            .stop_displaced_historical(&AgentProvider::OpenCode, &registry, Some(&row.worker))
            .await,
    ];
    for (caller, result) in [
        "shutdown_replay",
        "stop_historical_predecessor",
        "stop_displaced_historical",
    ]
    .into_iter()
    .zip(results)
    {
        let error = result.expect_err(caller);
        assert!(error.to_string().contains("pre-C4"), "{caller}: {error}");
    }
    // A newly created session was never part of the failed legacy sweep.
    let fresh = owner(&repo, root.path(), AgentProvider::OpenCode).await;
    super::lifecycle::stop_session(
        &host,
        &fresh.worker,
        crate::provider_execution::StopReason::Shutdown,
    )
    .await
    .unwrap();
}

mod pre_c4_sweep_tests;
