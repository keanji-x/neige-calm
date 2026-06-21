#![cfg(unix)]

mod support;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mcp_token_set_tx, session_projection_active_for_card_tx, session_start_runtime_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewCove, NewPlugin, NewWave, WaveId, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use support::mcp::{
    connect, handshake, recv_frame, send_frame, tools_call_frame, tools_list_frame,
};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
const PLUGIN_ID: &str = "dev.neige.git-forge";
const WORKTREE_TOOL: &str = "plugin.dev.neige.git-forge_git.worktree.add";
const COMMIT_TOOL: &str = "plugin.dev.neige.git-forge_git.commit";
const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";

static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct Fixture {
    _server: Arc<McpServer>,
    plugin_host: Arc<PluginHost>,
    repo: Arc<SqlxRepo>,
    socket_path: PathBuf,
    raw_token: String,
    thread_id: String,
    card_id: String,
    wave_id: String,
    lease_abs: PathBuf,
    _runtime: Arc<OperationRuntime>,
    _lease_tmp: TempDir,
    _tmp: TempDir,
}

struct Caller {
    raw_token: String,
    thread_id: String,
    card_id: String,
    wave_id: String,
    lease_abs: PathBuf,
    _lease_tmp: TempDir,
}

type EventRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Value,
);

type RawEventRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

#[test]
fn real_manifest_parses() {
    let manifest = read_manifest();
    assert_eq!(manifest.id, PLUGIN_ID);
    let tool_names = manifest
        .exposes_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            "git.worktree.add",
            "git.commit",
            "gh.pr.create",
            "gh.pr.list",
            "gh.pr.diff",
            "gh.pr.checks",
            "gh.pr.merge",
            "gh.issue.view",
            "gh.issue.close",
        ]
    );
}

#[tokio::test]
async fn real_git_forge_plugin_lowers_through_forge_action_seam() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = short_tempdir("gfr").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());

    let fx = boot_fixture().await;
    assert_tools_are_discoverable(&fx).await;

    let target = fx._tmp.path().join("worktree-target");
    let target = target.display().to_string();
    let worktree_resp = call_tool(
        &fx,
        10,
        WORKTREE_TOOL,
        json!({ "target": target, "branch": "wt-x" }),
    )
    .await;
    assert!(
        worktree_resp.get("error").is_none(),
        "worktree add returned JSON-RPC error: {worktree_resp:#?}"
    );
    assert_eq!(worktree_resp["result"]["isError"], false);
    let worktree_structured = &worktree_resp["result"]["structuredContent"];
    assert_eq!(worktree_structured["parked"], false);
    assert!(worktree_structured["op_id"].as_str().is_some());
    assert_eq!(
        worktree_structured["result"]["event_kind"],
        "worktree.provisioned"
    );
    assert_eq!(worktree_structured["result"]["event"]["path"], target);

    let rows = event_rows(&fx.repo, "worktree.provisioned").await;
    assert_eq!(rows.len(), 1, "worktree add must persist one event");
    assert_worktree_event(&rows[0], &fx.wave_id, &fx.card_id, &target);
    let worktree_key = scoped_idem_key(
        PLUGIN_ID,
        &fx.wave_id,
        &fx.card_id,
        &format!("git.worktree.add:{target}"),
    );
    assert!(
        operation_payload_by_idem(&fx.repo, &worktree_key)
            .await
            .is_some(),
        "worktree operation must use the kernel-scoped idempotency key"
    );

    stage_git_change(&fx.lease_abs, "change.txt", "change\n");
    let before_commit_events = forge_event_count(&fx.repo).await;
    let commit_resp = call_tool(
        &fx,
        11,
        COMMIT_TOOL,
        json!({ "message": "m", "idem": "step-1" }),
    )
    .await;
    assert!(
        commit_resp.get("error").is_none(),
        "git commit returned JSON-RPC error: {commit_resp:#?}"
    );
    assert_eq!(commit_resp["result"]["isError"], false);
    let commit_structured = &commit_resp["result"]["structuredContent"];
    assert_eq!(commit_structured["parked"], false);
    assert!(commit_structured["op_id"].as_str().is_some());
    assert!(commit_structured["result"]["event_kind"].is_null());
    assert!(commit_structured["result"]["event"].is_null());
    assert_eq!(
        forge_event_count(&fx.repo).await,
        before_commit_events,
        "resultless git.commit must not persist another forge event row"
    );

    let commit_key = scoped_idem_key(PLUGIN_ID, &fx.wave_id, &fx.card_id, "git.commit:step-1");
    assert!(
        operation_payload_by_idem(&fx.repo, &commit_key)
            .await
            .is_some(),
        "commit operation must use the kernel-scoped idempotency key"
    );

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

async fn boot_fixture() -> Fixture {
    let tmp = short_tempdir("mgf").expect("tempdir");
    let socket_path = tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let wave_cwd = tmp.path().join("wave-cwd");
    std::fs::create_dir_all(&wave_cwd).expect("create wave cwd");
    init_git_repo(&wave_cwd);

    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let events = EventBus::new();

    let cove = repo
        .cove_create(NewCove {
            name: "mcp-git-forge-plugin".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-git-forge-plugin".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    repo.seed_wave_cove_cache(&wave_cove_cache)
        .await
        .expect("seed wave/cove cache");

    let caller = create_worker_caller(&sqlx_repo, &card_role_cache, wave.id.clone()).await;
    init_git_repo(&caller.lease_abs);

    let plugin_host = boot_plugin_host(
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
    )
    .await;
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host).await;

    let operation_repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let runtime = Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events.clone(),
                completion,
            ),
        )
        .await
        .expect("operation runtime"),
    );

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    let operation_runtime_cell = Arc::new(OnceCell::new());
    assert!(operation_runtime_cell.set(runtime.clone()).is_ok());
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        plugin_host_cell,
        operation_runtime_cell,
        tmp.path().join("gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    Fixture {
        _server: server,
        plugin_host,
        repo: sqlx_repo,
        socket_path,
        raw_token: caller.raw_token,
        thread_id: caller.thread_id,
        card_id: caller.card_id,
        wave_id: caller.wave_id,
        lease_abs: caller.lease_abs,
        _runtime: runtime,
        _lease_tmp: caller._lease_tmp,
        _tmp: tmp,
    }
}

async fn create_worker_caller(
    sqlx_repo: &Arc<SqlxRepo>,
    card_role_cache: &CardRoleCache,
    wave_id: WaveId,
) -> Caller {
    let card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();
    let lease_tmp = tempfile::Builder::new()
        .prefix(".git-forge-lease-")
        .tempdir()
        .expect("worker lease tempdir");
    let lease_abs = lease_tmp.path().join("leases").join(&card_id);
    std::fs::create_dir_all(&lease_abs).expect("create lease dir");
    let lease_path = lease_abs.display().to_string();

    let mut tx = sqlx_repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave_id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint codex card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint card MCP token");
            session_mcp_token_set_tx(&mut tx, &runtime_id, &token_hash)
                .await
                .expect("mint session MCP token");
            token.into_inner()
        }
    };
    insert_workspace_lease(&mut tx, &card_id, wave_id.as_str(), &lease_path).await;
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    Caller {
        raw_token,
        thread_id,
        card_id,
        wave_id: wave_id.to_string(),
        lease_abs,
        _lease_tmp: lease_tmp,
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: calm_server::state::WriteContext,
) -> Arc<PluginHost> {
    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(FORGE_BIN), bin_dir.join("git-forge"))
        .expect("symlink git-forge plugin");

    let manifest = read_manifest();
    let manifest_json = manifest.to_json();
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    repo.plugin_install(NewPlugin {
        id: PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: manifest_json,
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");

    Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        write,
    ))
}

async fn insert_workspace_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    wave_id: &str,
    path: &str,
) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, NULL, ?7, ?7)"#,
    )
    .bind(calm_server::model::new_id())
    .bind(card_id)
    .bind(wave_id)
    .bind(path)
    .bind("test-lease-owner")
    .bind(now + 60_000)
    .bind(now)
    .execute(&mut **tx)
    .await
    .expect("insert workspace lease");
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    let runtime_id = if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, card_id)
        .await
        .expect("active runtime lookup")
    {
        let runtime_id = runtime.id.clone();
        session_bind_attribution_tx(
            &mut tx,
            &runtime_id,
            ThreadAttribution {
                runtime_id: runtime_id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .expect("bind thread attribution");
        runtime_id
    } else {
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
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
        .expect("start runtime");
        runtime.id
    };
    tx.commit().await.expect("commit runtime tx");
    runtime_id
}

async fn wait_for_running(host: &Arc<PluginHost>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(PLUGIN_ID).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_tools_are_discoverable(fx: &Fixture) {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(list.get("error").is_none(), "tools/list errored: {list:#?}");
    let names = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [WORKTREE_TOOL, COMMIT_TOOL, PR_CREATE_TOOL] {
        assert!(
            names.contains(&expected),
            "git-forge plugin tool missing from discovery: {expected}; got {names:?}"
        );
    }
}

async fn call_tool(fx: &Fixture, id: i64, name: &str, args: Value) -> Value {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_call_frame(id, name, &fx.thread_id, args)).await;
    recv_frame(&mut rd).await
}

async fn event_rows(repo: &SqlxRepo, kind: &str) -> Vec<EventRow> {
    let rows: Vec<RawEventRow> = sqlx::query_as(
        "SELECT scope_kind, scope_cove, scope_wave, scope_card, payload \
             FROM events WHERE kind = ?1 ORDER BY id ASC",
    )
    .bind(kind)
    .fetch_all(repo.pool())
    .await
    .expect("event rows");
    rows.into_iter()
        .map(
            |(scope_kind, scope_cove, scope_wave, scope_card, payload)| {
                (
                    scope_kind,
                    scope_cove,
                    scope_wave,
                    scope_card,
                    serde_json::from_str(&payload).expect("event payload json"),
                )
            },
        )
        .collect()
}

async fn forge_event_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind IN (\
         'forge.pr.merged', 'forge.scan.completed', 'forge.pr.opened', \
         'forge.pr.diff.read', 'forge.pr.checks', 'forge.issue.read', \
         'forge.issue.closed', 'worktree.provisioned', 'worktree.removed')",
    )
    .fetch_one(repo.pool())
    .await
    .expect("forge event count")
}

async fn operation_payload_by_idem(repo: &SqlxRepo, idem_key: &str) -> Option<Value> {
    let payload: Option<String> = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind = ?1 AND idempotency_key = ?2",
    )
    .bind(FORGE_ACTION_KIND)
    .bind(idem_key)
    .fetch_optional(repo.pool())
    .await
    .expect("operation payload lookup");
    payload.map(|payload| serde_json::from_str(&payload).expect("operation payload json"))
}

fn assert_worktree_event(row: &EventRow, wave_id: &str, card_id: &str, target: &str) {
    let (scope_kind, _scope_cove, scope_wave, scope_card, payload) = row;
    assert_eq!(scope_kind, "card");
    assert_eq!(scope_wave.as_deref(), Some(wave_id));
    assert_eq!(scope_card.as_deref(), Some(card_id));
    assert_eq!(payload["wave_id"], wave_id);
    assert_eq!(payload["card_id"], card_id);
    assert_eq!(payload["path"], target);
}

fn scoped_idem_key(plugin_id: &str, wave_id: &str, card_id: &str, idem_key: &str) -> String {
    format!("{plugin_id}:{wave_id}:{card_id}:{idem_key}")
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-forge/manifest.json")
}

fn read_manifest() -> Manifest {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    Manifest::parse(&raw).expect("git-forge manifest parses")
}

fn init_git_repo(path: &Path) {
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "git-forge@example.test"]);
    run_git(path, ["config", "user.name", "Git Forge Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write initial file");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn stage_git_change(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).expect("write git change");
    run_git(repo, ["add", name]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn short_tempdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("mfa-tmp");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
