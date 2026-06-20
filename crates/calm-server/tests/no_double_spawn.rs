#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_with_claude_create_tx, card_with_codex_create_tx, card_with_terminal_create_tx,
};
use calm_server::db::write_in_tx_typed;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{Card, CardPatch, CardRole, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::claude_adapter::{
    ClaudeAdapter, ClaudeCreateOperationPayload, PreparedClaudeCreateRequest,
};
use calm_server::operation::codex_adapter::{
    CodexAdapter, CodexCreateOperationPayload, CodexWorkerAdapter, CodexWorkerOperationPayload,
    NormalizedCodexCreateRequest,
};
use calm_server::operation::terminal_adapter::{
    TerminalAdapter, TerminalCreateOperationPayload, TerminalCreateRequestPayload,
    TerminalWorkerAdapter, TerminalWorkerOperationPayload,
};
use calm_server::operation::{
    CompensationStateVersioned, Operation, OperationCompletionBus, OperationKey, OperationOutcome,
    OperationRepo, OperationResult, OperationRuntime, Phase, PhaseTag, ProviderAdapter,
    RecoveryItem, SpawnCtx, SpawnHandle, SqlxOperationRepo, TxOutput,
};
use calm_server::pending_codex_threads::PendingThreadStartRegistry;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_lookup::project_runtime_into_card_payload;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionProjectionRepo};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    state: AppState,
    repo: Arc<SqlxRepo>,
    wave_id: String,
    spawn_count: Arc<AtomicUsize>,
    _tmp: TempDir,
}

type TestTerminalWorkerSpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, CalmResult<SpawnHandle>>
        + Send
        + Sync,
>;

struct DriveErrorOnceRepo {
    inner: SqlxOperationRepo,
    fail_next_drive: AtomicBool,
    drive_failures: AtomicUsize,
}

impl DriveErrorOnceRepo {
    fn new(inner: SqlxOperationRepo) -> Self {
        Self {
            inner,
            fail_next_drive: AtomicBool::new(true),
            drive_failures: AtomicUsize::new(0),
        }
    }

    fn drive_failures(&self) -> usize {
        self.drive_failures.load(Ordering::SeqCst)
    }
}

struct ReverseSpawnClaimRepo {
    inner: SqlxOperationRepo,
    inserted_count: AtomicUsize,
    gate_until_inserts: usize,
}

impl ReverseSpawnClaimRepo {
    fn new(inner: SqlxOperationRepo, gate_until_inserts: usize) -> Self {
        Self {
            inner,
            inserted_count: AtomicUsize::new(0),
            gate_until_inserts,
        }
    }
}

#[async_trait]
impl OperationRepo for ReverseSpawnClaimRepo {
    fn sqlite_pool(&self) -> sqlx::SqlitePool {
        self.inner.sqlite_pool()
    }

    async fn assert_sqlite_version(&self) -> CalmResult<()> {
        self.inner.assert_sqlite_version().await
    }

    async fn insert_operation(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> CalmResult<String> {
        let id = self.inner.insert_operation(kind, key, payload).await?;
        self.inserted_count.fetch_add(1, Ordering::SeqCst);
        Ok(id)
    }

    async fn find_by_idempotency_key(
        &self,
        kind: &str,
        key: &OperationKey,
    ) -> CalmResult<Option<Operation>> {
        self.inner.find_by_idempotency_key(kind, key).await
    }

    async fn get_operation(&self, op_id: &str) -> CalmResult<Option<Operation>> {
        self.inner.get_operation(op_id).await
    }

    async fn operation_result(&self, op_id: &str) -> CalmResult<Option<OperationResult>> {
        self.inner.operation_result(op_id).await
    }

    async fn claim_drive_batch(&self, limit: i64) -> CalmResult<Vec<Operation>> {
        if self.inserted_count.load(Ordering::SeqCst) < self.gate_until_inserts {
            return Ok(Vec::new());
        }
        let mut batch = self.inner.claim_drive_batch(limit).await?;
        if batch
            .iter()
            .any(|op| matches!(&op.phase, Phase::SpawnStarted))
        {
            batch.reverse();
        }
        Ok(batch)
    }

    async fn abandoned_running_operations_on_boot(&self) -> CalmResult<Vec<Operation>> {
        self.inner.abandoned_running_operations_on_boot().await
    }

    async fn abandoned_running_operations_steady_state(&self) -> CalmResult<Vec<Operation>> {
        self.inner.abandoned_running_operations_steady_state().await
    }

    async fn claim_operation_for_recovery(&self, op_id: &str) -> CalmResult<Option<Operation>> {
        self.inner.claim_operation_for_recovery(op_id).await
    }

    async fn prepare_tx_and_advance(
        &self,
        op: &Operation,
        adapter: &dyn ProviderAdapter,
    ) -> CalmResult<Option<(Operation, Vec<BroadcastEnvelope>)>> {
        self.inner.prepare_tx_and_advance(op, adapter).await
    }

    async fn set_phase(&self, op: &Operation, phase: Phase) -> CalmResult<Option<Operation>> {
        self.inner.set_phase(op, phase).await
    }

    async fn set_phase_and_tx_output(
        &self,
        op: &Operation,
        phase: Phase,
        output: &TxOutput,
    ) -> CalmResult<Option<Operation>> {
        self.inner.set_phase_and_tx_output(op, phase, output).await
    }

    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
        output: &TxOutput,
    ) -> CalmResult<Option<Operation>> {
        self.inner.set_compensating(op, state, output).await
    }

    async fn update_compensation_state(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> CalmResult<Option<Operation>> {
        self.inner.update_compensation_state(op, state).await
    }

    async fn mark_failed(
        &self,
        op: &Operation,
        last_error: String,
        from_phase: calm_server::operation::PhaseTag,
        last_error_class: Option<String>,
    ) -> CalmResult<Option<OperationResult>> {
        self.inner
            .mark_failed(op, last_error, from_phase, last_error_class)
            .await
    }

    async fn mark_stuck(
        &self,
        op: &Operation,
        reason: String,
        from_phase: calm_server::operation::PhaseTag,
    ) -> CalmResult<Option<OperationResult>> {
        self.inner.mark_stuck(op, reason, from_phase).await
    }
}

#[async_trait]
impl OperationRepo for DriveErrorOnceRepo {
    fn sqlite_pool(&self) -> sqlx::SqlitePool {
        self.inner.sqlite_pool()
    }

    async fn assert_sqlite_version(&self) -> CalmResult<()> {
        self.inner.assert_sqlite_version().await
    }

    async fn insert_operation(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> CalmResult<String> {
        self.inner.insert_operation(kind, key, payload).await
    }

    async fn find_by_idempotency_key(
        &self,
        kind: &str,
        key: &OperationKey,
    ) -> CalmResult<Option<Operation>> {
        self.inner.find_by_idempotency_key(kind, key).await
    }

    async fn get_operation(&self, op_id: &str) -> CalmResult<Option<Operation>> {
        self.inner.get_operation(op_id).await
    }

    async fn operation_result(&self, op_id: &str) -> CalmResult<Option<OperationResult>> {
        self.inner.operation_result(op_id).await
    }

    async fn claim_drive_batch(&self, limit: i64) -> CalmResult<Vec<Operation>> {
        if self.fail_next_drive.swap(false, Ordering::SeqCst) {
            self.drive_failures.fetch_add(1, Ordering::SeqCst);
            return Err(CalmError::Internal("forced drive failure".into()));
        }
        self.inner.claim_drive_batch(limit).await
    }

    async fn abandoned_running_operations_on_boot(&self) -> CalmResult<Vec<Operation>> {
        self.inner.abandoned_running_operations_on_boot().await
    }

    async fn abandoned_running_operations_steady_state(&self) -> CalmResult<Vec<Operation>> {
        self.inner.abandoned_running_operations_steady_state().await
    }

    async fn claim_operation_for_recovery(&self, op_id: &str) -> CalmResult<Option<Operation>> {
        self.inner.claim_operation_for_recovery(op_id).await
    }

    async fn prepare_tx_and_advance(
        &self,
        op: &Operation,
        adapter: &dyn ProviderAdapter,
    ) -> CalmResult<Option<(Operation, Vec<BroadcastEnvelope>)>> {
        self.inner.prepare_tx_and_advance(op, adapter).await
    }

    async fn set_phase(&self, op: &Operation, phase: Phase) -> CalmResult<Option<Operation>> {
        self.inner.set_phase(op, phase).await
    }

    async fn set_phase_and_tx_output(
        &self,
        op: &Operation,
        phase: Phase,
        output: &TxOutput,
    ) -> CalmResult<Option<Operation>> {
        self.inner.set_phase_and_tx_output(op, phase, output).await
    }

    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
        output: &TxOutput,
    ) -> CalmResult<Option<Operation>> {
        self.inner.set_compensating(op, state, output).await
    }

    async fn update_compensation_state(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> CalmResult<Option<Operation>> {
        self.inner.update_compensation_state(op, state).await
    }

    async fn mark_failed(
        &self,
        op: &Operation,
        last_error: String,
        from_phase: calm_server::operation::PhaseTag,
        last_error_class: Option<String>,
    ) -> CalmResult<Option<OperationResult>> {
        self.inner
            .mark_failed(op, last_error, from_phase, last_error_class)
            .await
    }

    async fn mark_stuck(
        &self,
        op: &Operation,
        reason: String,
        from_phase: calm_server::operation::PhaseTag,
    ) -> CalmResult<Option<OperationResult>> {
        self.inner.mark_stuck(op, reason, from_phase).await
    }
}

async fn boot_with_counted_spawn() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let cove = repo_dyn
        .cove_create(NewCove {
            name: "operations-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo_dyn
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "operations-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let spawn_count = Arc::new(AtomicUsize::new(0));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo_dyn.clone();
    let count_for_hook = spawn_count.clone();
    let repo_for_hook = route_repo.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let repo = repo_for_hook.clone();
            Box::pin(async move {
                let spawn_index = count.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_pid(&terminal_id, Some(48_100 + spawn_index as u32))
                    .await?;
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    );
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let terminal_adapter = Arc::new(TerminalAdapter::new_with_spawn_hook(
        route_repo.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![terminal_adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
            completion,
        ),
    ));
    let state = state.with_operation_runtime(runtime);

    Boot {
        state,
        repo,
        wave_id: wave.id.to_string(),
        spawn_count,
        _tmp: tmp,
    }
}

async fn boot_codex_with_counted_spawn() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let cove = repo_dyn
        .cove_create(NewCove {
            name: "codex-operations-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo_dyn
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "codex-operations-test".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let codex = Arc::new(CodexClient::new_stub());
    let mut state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        codex.clone(),
        None,
        None,
    );
    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo_dyn.clone(),
        events.clone(),
    ));
    let shared = SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn.clone(),
        Some(pending.clone()),
    );
    state = state.with_shared_codex_appserver(shared);
    state = state.with_pending_codex_threads(pending);

    let spawn_count = Arc::new(AtomicUsize::new(0));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo_dyn.clone();
    let count_for_hook = spawn_count.clone();
    let repo_for_hook = route_repo.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let repo = repo_for_hook.clone();
            Box::pin(async move {
                let spawn_index = count.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_pid(&terminal_id, Some(58_100 + spawn_index as u32))
                    .await?;
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    );
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let terminal_adapter = Arc::new(TerminalAdapter::new(
        route_repo.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
    ));
    let codex_adapter = Arc::new(CodexAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex,
        state.shared_codex_appserver.clone(),
        state.pending_codex_threads.clone(),
        state.pending_codex_threads_spawn_serial.clone(),
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![terminal_adapter, codex_adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
            completion,
        ),
    ));
    let state = state.with_operation_runtime(runtime);

    Boot {
        state,
        repo,
        wave_id: wave.id.to_string(),
        spawn_count,
        _tmp: tmp,
    }
}

async fn boot_claude_with_counted_spawn() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let cove = repo_dyn
        .cove_create(NewCove {
            name: "claude-operations-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo_dyn
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "claude-operations-test".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let mut codex = CodexClient::new_stub();
    codex.claude_bin = "/bin/true".into();
    codex.ingest_url = "http://127.0.0.1:4040".into();
    let codex = Arc::new(codex);
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        codex.clone(),
        None,
        None,
    );

    let spawn_count = Arc::new(AtomicUsize::new(0));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo_dyn.clone();
    let count_for_hook = spawn_count.clone();
    let repo_for_hook = route_repo.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let repo = repo_for_hook.clone();
            Box::pin(async move {
                let spawn_index = count.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_pid(&terminal_id, Some(78_100 + spawn_index as u32))
                    .await?;
                Ok(SpawnHandle::Terminal {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
                })
            })
        },
    );
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let claude_adapter = Arc::new(ClaudeAdapter::new_with_spawn_hook(
        route_repo.clone(),
        codex,
        state.card_role_cache.clone(),
        state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![claude_adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
            completion,
        ),
    ));
    let state = state.with_operation_runtime(runtime);

    Boot {
        state,
        repo,
        wave_id: wave.id.to_string(),
        spawn_count,
        _tmp: tmp,
    }
}

async fn boot_codex_with_reversed_spawn_claims_and_thread_notifications() -> Boot {
    let mut boot = boot_codex_with_counted_spawn().await;
    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo_dyn.clone();
    let count_for_hook = boot.spawn_count.clone();
    let repo_for_hook = route_repo.clone();
    let shared_for_hook = boot.state.shared_codex_appserver.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            let repo = repo_for_hook.clone();
            let shared = shared_for_hook.clone();
            Box::pin(async move {
                let spawn_index = count.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_pid(&terminal_id, Some(68_100 + spawn_index as u32))
                    .await?;
                let term = repo.terminal_get(&terminal_id).await?.ok_or_else(|| {
                    CalmError::Internal(format!("terminal {terminal_id} vanished"))
                })?;
                let thread_id = format!("thread-for-{}", term.card_id);
                if !shared
                    .handle_thread_started_notification_for_test(&thread_id)
                    .await?
                {
                    return Err(CalmError::Internal(format!(
                        "thread/started {thread_id} was not consumed by pending FIFO"
                    )));
                }
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    );
    let operation_repo = Arc::new(ReverseSpawnClaimRepo::new(
        SqlxOperationRepo::new(boot.repo.pool().clone()),
        2,
    ));
    let codex_adapter = Arc::new(CodexAdapter::new_with_spawn_hook(
        route_repo.clone(),
        boot.state.codex.clone(),
        boot.state.shared_codex_appserver.clone(),
        boot.state.pending_codex_threads.clone(),
        boot.state.pending_codex_threads_spawn_serial.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![codex_adapter],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.with_operation_runtime(runtime);
    boot
}

#[tokio::test]
async fn test_terminal_create_no_double_spawn() {
    let boot = boot_with_counted_spawn().await;
    let payload = terminal_payload(&boot.wave_id);
    let key = OperationKey {
        operation_key: "op-terminal-create".into(),
        idempotency_key: Some("terminal-create-same-key".into()),
        payload_hash: "same-payload-hash".into(),
    };

    let rt_a = boot.state.operation_runtime.clone();
    let rt_b = boot.state.operation_runtime.clone();
    let payload_a = payload.clone();
    let payload_b = payload;
    let key_a = key.clone();
    let key_b = key;
    let a = tokio::spawn(async move {
        let op_id = rt_a
            .submit("terminal-create", key_a, payload_a)
            .await
            .unwrap();
        rt_a.wait(&op_id).await.unwrap()
    });
    let b = tokio::spawn(async move {
        let op_id = rt_b
            .submit("terminal-create", key_b, payload_b)
            .await
            .unwrap();
        rt_b.wait(&op_id).await.unwrap()
    });
    let (a, b) = tokio::join!(a, b);
    let a = a.unwrap();
    let b = b.unwrap();
    let card_a = result_card_id(&a.outcome);
    let card_b = result_card_id(&b.outcome);

    assert_eq!(card_a, card_b);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    let row = sqlx::query("SELECT COUNT(*) AS n FROM operations")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let count: i64 = row.try_get("n").unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_codex_create_no_double_spawn() {
    let boot = boot_codex_with_counted_spawn().await;
    let payload = codex_payload(&boot.wave_id, None);
    let key = OperationKey {
        operation_key: "op-codex-create".into(),
        idempotency_key: Some("codex-create-same-key".into()),
        payload_hash: "same-codex-payload-hash".into(),
    };

    let rt_a = boot.state.operation_runtime.clone();
    let rt_b = boot.state.operation_runtime.clone();
    let payload_a = payload.clone();
    let payload_b = payload;
    let key_a = key.clone();
    let key_b = key;
    let a = tokio::spawn(async move {
        let op_id = rt_a.submit("codex-create", key_a, payload_a).await.unwrap();
        rt_a.wait(&op_id).await.unwrap()
    });
    let b = tokio::spawn(async move {
        let op_id = rt_b.submit("codex-create", key_b, payload_b).await.unwrap();
        rt_b.wait(&op_id).await.unwrap()
    });
    let (a, b) = tokio::join!(a, b);
    let a = a.unwrap();
    let b = b.unwrap();
    let card_a = result_card_id(&a.outcome);
    let card_b = result_card_id(&b.outcome);

    assert_eq!(card_a, card_b);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    let row = sqlx::query("SELECT COUNT(*) AS n FROM operations WHERE kind = 'codex-create'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let count: i64 = row.try_get("n").unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_claude_create_no_double_spawn() {
    let boot = boot_claude_with_counted_spawn().await;
    let payload = claude_payload(&boot, &boot.wave_id, None);
    let key = OperationKey {
        operation_key: "op-claude-create".into(),
        idempotency_key: Some("claude-create-same-key".into()),
        payload_hash: "same-claude-payload-hash".into(),
    };

    let rt_a = boot.state.operation_runtime.clone();
    let rt_b = boot.state.operation_runtime.clone();
    let payload_a = payload.clone();
    let payload_b = payload;
    let key_a = key.clone();
    let key_b = key;
    let a = tokio::spawn(async move {
        let op_id = rt_a
            .submit("claude-create", key_a, payload_a)
            .await
            .unwrap();
        rt_a.wait(&op_id).await.unwrap()
    });
    let b = tokio::spawn(async move {
        let op_id = rt_b
            .submit("claude-create", key_b, payload_b)
            .await
            .unwrap();
        rt_b.wait(&op_id).await.unwrap()
    });
    let (a, b) = tokio::join!(a, b);
    let a = a.unwrap();
    let b = b.unwrap();
    let card_a = result_card_id(&a.outcome);
    let card_b = result_card_id(&b.outcome);

    assert_eq!(card_a, card_b);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    let row = sqlx::query("SELECT COUNT(*) AS n FROM operations WHERE kind = 'claude-create'")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let count: i64 = row.try_get("n").unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn codex_empty_concurrent_creates_bind_fifo_to_spawn_order() {
    let boot = boot_codex_with_reversed_spawn_claims_and_thread_notifications().await;
    let payload_a = codex_payload(&boot.wave_id, None);
    let payload_b = codex_payload(&boot.wave_id, None);
    let key_a = OperationKey {
        operation_key: "op-codex-empty-fifo-a".into(),
        idempotency_key: Some("codex-empty-fifo-a".into()),
        payload_hash: "codex-empty-fifo-hash-a".into(),
    };
    let key_b = OperationKey {
        operation_key: "op-codex-empty-fifo-b".into(),
        idempotency_key: Some("codex-empty-fifo-b".into()),
        payload_hash: "codex-empty-fifo-hash-b".into(),
    };

    let rt_a = boot.state.operation_runtime.clone();
    let rt_b = boot.state.operation_runtime.clone();
    let a = tokio::spawn(async move {
        let op_id = rt_a.submit("codex-create", key_a, payload_a).await.unwrap();
        rt_a.wait(&op_id).await.unwrap()
    });
    let b = tokio::spawn(async move {
        let op_id = rt_b.submit("codex-create", key_b, payload_b).await.unwrap();
        rt_b.wait(&op_id).await.unwrap()
    });
    let (a, b) = tokio::join!(a, b);
    let card_a = result_card_id(&a.unwrap().outcome);
    let card_b = result_card_id(&b.unwrap().outcome);

    assert_ne!(card_a, card_b);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 2);
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);

    for card_id in [&card_a, &card_b] {
        let expected_thread_id = format!("thread-for-{card_id}");
        assert_active_codex_runtime_thread(&boot, card_id, &expected_thread_id).await;
        let mut card = boot
            .repo
            .card_get(card_id)
            .await
            .unwrap()
            .expect("card row");
        assert!(card.payload.get("codex_thread_id").is_none());
        project_runtime_into_card_payload(boot.repo.as_ref(), &mut card)
            .await
            .unwrap();
        assert_eq!(card.payload["codex_thread_id"], expected_thread_id);
        assert_eq!(card.payload["codex_thread_status"], "started");
    }
}

async fn assert_active_codex_runtime_thread(boot: &Boot, card_id: &str, expected_thread_id: &str) {
    let runtime = boot
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await
        .unwrap()
        .expect("active runtime row");
    assert_eq!(runtime.kind, WorkerSessionKind::CodexCard);
    assert_eq!(runtime.thread_id.as_deref(), Some(expected_thread_id));
}

async fn force_codex_runtime_thread_running(boot: &Boot, card_id: &str, thread_id: &str) {
    sqlx::query(
        r#"UPDATE worker_sessions
           SET state = 'running',
               thread_id = ?1
           WHERE card_id = ?2"#,
    )
    .bind(thread_id)
    .bind(card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();
}

async fn assert_projected_codex_thread(
    boot: &Boot,
    card_id: &str,
    expected_thread_id: &str,
    expected_status: &str,
) {
    assert_active_codex_runtime_thread(boot, card_id, expected_thread_id).await;
    let mut card = boot
        .repo
        .card_get(card_id)
        .await
        .unwrap()
        .expect("card row");
    project_runtime_into_card_payload(boot.repo.as_ref(), &mut card)
        .await
        .unwrap();
    assert_eq!(card.payload["codex_thread_id"], expected_thread_id);
    assert_eq!(card.payload["codex_thread_status"], expected_status);
}

async fn assert_raw_payload_has_no_codex_thread_id(boot: &Boot, card_id: &str) {
    let card = boot
        .repo
        .card_get(card_id)
        .await
        .unwrap()
        .expect("card row");
    assert!(card.payload.get("codex_thread_id").is_none());
}

#[tokio::test]
async fn terminal_create_same_idempotency_key_different_actor_conflicts() {
    let boot = boot_with_counted_spawn().await;
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone());
    let body = terminal_route_body();

    let (first_status, first_body) = post_terminal_card_route(
        app.clone(),
        &boot.wave_id,
        body.clone(),
        Some("same-key-different-actor"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_body:?}");

    let (second_status, second_body) = post_terminal_card_route(
        app,
        &boot.wave_id,
        body,
        Some("same-key-different-actor"),
        Some("ai:codex"),
    )
    .await;
    assert_eq!(
        second_status,
        StatusCode::CONFLICT,
        "same key/body with different actor must conflict: {second_body:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_create_same_idempotency_key_equivalent_normalized_body_reuses_operation() {
    let boot = boot_with_counted_spawn().await;
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone());
    let mut first_body = terminal_route_body();
    first_body["program"] = json!("  bash  ");
    first_body["env"] = Value::Null;
    let mut second_body = terminal_route_body();
    second_body["program"] = json!("bash");
    second_body["env"] = json!({});

    let (first_status, first_body) = post_terminal_card_route(
        app.clone(),
        &boot.wave_id,
        first_body,
        Some("same-key-normalized-equivalent"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_body:?}");

    let (second_status, second_body) = post_terminal_card_route(
        app,
        &boot.wave_id,
        second_body,
        Some("same-key-normalized-equivalent"),
        None,
    )
    .await;
    assert_eq!(
        second_status,
        StatusCode::CREATED,
        "equivalent normalized body must reuse existing operation: {second_body:?}"
    );
    assert_eq!(
        response_card_id(&first_body),
        response_card_id(&second_body)
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_create_same_idempotency_key_different_normalized_env_conflicts() {
    let boot = boot_with_counted_spawn().await;
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone());
    let mut first_body = terminal_route_body();
    first_body["program"] = json!("bash");
    first_body["env"] = json!({ "FOO": "bar" });
    let mut second_body = terminal_route_body();
    second_body["program"] = json!("bash");
    second_body["env"] = json!({ "BAZ": "qux" });

    let (first_status, first_body) = post_terminal_card_route(
        app.clone(),
        &boot.wave_id,
        first_body,
        Some("same-key-different-normalized-env"),
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first_body:?}");

    let (second_status, second_body) = post_terminal_card_route(
        app,
        &boot.wave_id,
        second_body,
        Some("same-key-different-normalized-env"),
        None,
    )
    .await;
    assert_eq!(
        second_status,
        StatusCode::CONFLICT,
        "different normalized env must conflict: {second_body:?}"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_create_recovery_from_tx_committed_replays_spawn_once() {
    let boot = boot_with_counted_spawn().await;
    let card_id = new_id();
    let runtime_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                None,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "terminal_id": term.id,
        "program": "/bin/sh",
        "cwd": "/tmp",
        "env": {},
    });
    boot.repo
        .terminal_set_pid(&term.id, Some(12_345))
        .await
        .unwrap();
    boot.repo
        .terminal_set_exit(&term.id, Some(-1), false)
        .await
        .unwrap();
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("recovery-op")
    .bind("recovery-key")
    .bind("recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&terminal_payload(&boot.wave_id)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::TxCommitted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let row = sqlx::query("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    assert_eq!(phase, "succeeded");
    let term = boot.repo.terminal_get(&term.id).await.unwrap().unwrap();
    assert_eq!(term.exit_code, None);
    assert!(!term.signal_killed);
    assert_eq!(term.pid, Some(48_100));
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_create_recovery_from_tx_committed_reaches_terminal_phase() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let runtime_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let env = json!({
        "CODEX_HOME": boot.state.codex.codex_home_dir().to_string_lossy().to_string(),
        "NEIGE_CARD_ID": card_id.clone(),
        "NEIGE_CALM_BASE_URL": boot.state.codex.ingest_url,
    });
    let env_for_output = env.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term, _token) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_codex_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                None,
                wave_id.into(),
                None,
                "/workspace".into(),
                env.clone(),
                None,
                None,
                None,
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": term.id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": Value::Null,
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("codex-recovery-op")
    .bind("codex-recovery-key")
    .bind("codex-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, None)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::TxCommitted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    assert_eq!(phase, PhaseTag::Succeeded);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn claude_create_recovery_from_tx_committed_reaches_terminal_phase_and_writes_settings() {
    let boot = boot_claude_with_counted_spawn().await;
    let card_id = new_id();
    let runtime_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let claude_session_id = uuid::Uuid::new_v4().to_string();
    let settings_path = boot
        .state
        .codex
        .claude_settings_dir
        .join(&card_id)
        .join("settings.json")
        .to_string_lossy()
        .to_string();
    let command_line = format!(
        "/bin/true --settings {} --session-id {}",
        settings_path, claude_session_id
    );
    let env = json!({
        "NEIGE_CARD_ID": card_id.clone(),
        "NEIGE_CALM_BASE_URL": boot.state.codex.ingest_url,
        "NEIGE_HOOK_PROVIDER": "claude",
    });
    let env_for_output = env.clone();
    let settings_path_for_tx = settings_path.clone();
    let claude_session_id_for_tx = claude_session_id.clone();
    let command_line_for_tx = command_line.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_claude_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                wave_id.into(),
                None,
                command_line_for_tx,
                "/workspace".into(),
                env,
                None,
                None,
                None,
                settings_path_for_tx,
                claude_session_id_for_tx,
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    assert!(
        !std::path::Path::new(&settings_path).exists(),
        "seeded TxCommitted row should not pre-create settings.json"
    );
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": term.id,
        "settings_path": settings_path,
        "claude_session_id": claude_session_id,
        "command_line": command_line,
        "cwd": "/workspace",
        "env": env_for_output,
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'claude-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("claude-recovery-op")
    .bind("claude-recovery-key")
    .bind("claude-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&claude_payload(&boot, &boot.wave_id, None)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::TxCommitted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    assert_eq!(phase, PhaseTag::Succeeded);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    let output = SqlxOperationRepo::new(boot.repo.pool().clone())
        .get_operation(&op_id)
        .await
        .unwrap()
        .unwrap()
        .tx_output
        .unwrap();
    let settings_path = output
        .data
        .get("settings_path")
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        std::path::Path::new(settings_path).exists(),
        "recovery spawn must write settings.json"
    );
}

#[tokio::test]
async fn codex_prompt_recovery_from_tx_committed_reaches_terminal_phase() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, Some("recover prompt")).await;
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": "recover prompt",
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("codex-prompt-tx-committed-recovery-op")
    .bind("codex-prompt-tx-committed-recovery-key")
    .bind("codex-prompt-tx-committed-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, Some("recover prompt"))).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::TxCommitted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let row = sqlx::query("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    assert_eq!(phase, "succeeded");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    assert_raw_payload_has_no_codex_thread_id(&boot, card.id.as_str()).await;
    assert_projected_codex_thread(&boot, card.id.as_str(), "fake-thread-0001", "started").await;
}

#[tokio::test]
async fn codex_prompt_recovery_from_app_server_interact_reuses_existing_thread_mapping() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, Some("recover prompt")).await;
    force_codex_runtime_thread_running(&boot, card.id.as_str(), "T-original").await;
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": "recover prompt",
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, phase_detail_json, lease_owner, lease_until_ms,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8,
                   'app_server_interact', ?9, ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("codex-app-interact-recovery-op")
    .bind("codex-app-interact-recovery-key")
    .bind("codex-app-interact-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, Some("recover prompt"))).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(
        serde_json::to_string(&json!({
            "kind": "mint_and_await",
            "thread_id": Value::Null,
        }))
        .unwrap(),
    )
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::AppServerInteract { .. },
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let row = sqlx::query("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    assert_eq!(phase, "succeeded");
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    assert_active_codex_runtime_thread(&boot, card.id.as_str(), "T-original").await;
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0001"),
        None,
        "recovery must not mint a second shared thread"
    );
    assert_projected_codex_thread(&boot, card.id.as_str(), "T-original", "started").await;
}

#[tokio::test]
async fn codex_prompt_recovery_with_turn_started_marker_waits_for_lifecycle_without_replay() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, Some("recover prompt")).await;
    let mut payload = card.payload.clone();
    payload["codex_thread_id"] = json!("t1");
    let card = boot
        .repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(payload),
                deletable: None,
            },
        )
        .await
        .unwrap();
    force_codex_runtime_thread_running(&boot, card.id.as_str(), "t1").await;
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": "recover prompt",
        "codex_thread_id": "t1",
        "turn_started_at_ms": now_ms() - 1_000,
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, phase_detail_json, lease_owner, lease_until_ms,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8,
                   'app_server_interact', ?9, ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("codex-turn-started-marker-recovery-op")
    .bind("codex-turn-started-marker-recovery-key")
    .bind("codex-turn-started-marker-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, Some("recover prompt"))).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(
        serde_json::to_string(&json!({
            "kind": "mint_and_await",
            "thread_id": "t1",
        }))
        .unwrap(),
    )
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::AppServerInteract { .. },
            ..
        } if planned == &op_id
    ));
    let rt = boot.state.operation_runtime.clone();
    let recovery = tokio::spawn(async move {
        rt.apply_recovery(plan).await.unwrap();
    });
    wait_for_notification_receivers(&boot.state.shared_codex_appserver, 1).await;
    boot.state
        .shared_codex_appserver
        .emit_turn_started_for_test("t1", "recovered-turn");
    recovery.await.unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    assert_eq!(phase, PhaseTag::Succeeded);
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .turn_start_count_for_test(),
        0,
        "recovery must not replay turn_start after marker persistence"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_prompt_recovery_without_marker_replays_turn_start_idempotently() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, Some("recover prompt")).await;
    let mut payload = card.payload.clone();
    payload["codex_thread_id"] = json!("t1");
    let card = boot
        .repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(payload),
                deletable: None,
            },
        )
        .await
        .unwrap();
    force_codex_runtime_thread_running(&boot, card.id.as_str(), "t1").await;
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": "recover prompt",
        "codex_thread_id": "t1",
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, phase_detail_json, lease_owner, lease_until_ms,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8,
                   'app_server_interact', ?9, ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("codex-turn-start-replay-recovery-op")
    .bind("codex-turn-start-replay-recovery-key")
    .bind("codex-turn-start-replay-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, Some("recover prompt"))).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(
        serde_json::to_string(&json!({
            "kind": "mint_and_await",
            "thread_id": "t1",
        }))
        .unwrap(),
    )
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::AppServerInteract { .. },
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    let row = sqlx::query("SELECT tx_output_json FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let tx_output_json: String = row.try_get("tx_output_json").unwrap();
    let recovered_output: TxOutput = serde_json::from_str(&tx_output_json).unwrap();
    assert_eq!(phase, PhaseTag::Succeeded);
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .turn_start_count_for_test(),
        1,
        "recovery must replay turn_start when no post-call marker was checkpointed"
    );
    assert!(
        output_optional_i64_for_test(&recovered_output, "turn_started_at_ms").is_some(),
        "recovery must checkpoint the post-call turn_start marker after replay"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_prompt_recovery_with_turn_started_marker_times_out_without_lifecycle() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, Some("recover prompt")).await;
    let mut payload = card.payload.clone();
    payload["codex_thread_id"] = json!("t1");
    let card = boot
        .repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(payload),
                deletable: None,
            },
        )
        .await
        .unwrap();
    force_codex_runtime_thread_running(&boot, card.id.as_str(), "t1").await;
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": "recover prompt",
        "codex_thread_id": "t1",
        "turn_started_at_ms": now_ms() - 1_000,
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, phase_detail_json, lease_owner, lease_until_ms,
               created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8,
                   'app_server_interact', ?9, ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("codex-turn-started-marker-timeout-recovery-op")
    .bind("codex-turn-started-marker-timeout-recovery-key")
    .bind("codex-turn-started-marker-timeout-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, Some("recover prompt"))).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(
        serde_json::to_string(&json!({
            "kind": "mint_and_await",
            "thread_id": "t1",
        }))
        .unwrap(),
    )
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    let rt = boot.state.operation_runtime.clone();
    let recovery = tokio::spawn(async move {
        rt.apply_recovery(plan).await.unwrap();
    });
    wait_for_notification_receivers(&boot.state.shared_codex_appserver, 1).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    recovery.await.unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    assert_eq!(phase, PhaseTag::Failed);
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .turn_start_count_for_test(),
        0,
        "marker-present recovery must await lifecycle without replaying turn_start"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn codex_empty_recovery_from_spawn_started_rehydrates_pending_registry() {
    let boot = boot_codex_with_counted_spawn().await;
    let card_id = new_id();
    let (card, terminal_id, env_for_output, runtime_id) =
        seed_codex_card_for_operation(&boot, card_id, None).await;
    let mut pending_payload = card.payload.clone();
    pending_payload["codex_thread_status"] = json!("pending_thread_start");
    let card = boot
        .repo
        .card_update(
            card.id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(pending_payload),
                deletable: None,
            },
        )
        .await
        .unwrap();
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "wave_id": boot.wave_id.clone(),
        "terminal_id": terminal_id,
        "cwd": "/workspace",
        "env": env_for_output,
        "prompt": Value::Null,
        "pending_registered": true,
    });
    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'spawn_started', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("codex-empty-spawn-started-recovery-op")
    .bind("codex-empty-spawn-started-recovery-key")
    .bind("codex-empty-spawn-started-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&codex_payload(&boot.wave_id, None)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 0);
    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::SpawnStarted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let phase = wait_for_terminal_phase(&boot, &op_id, Duration::from_secs(5)).await;
    assert_eq!(phase, PhaseTag::Succeeded);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
    assert_eq!(boot.state.pending_codex_threads.pending_count().await, 1);
    assert!(
        boot.state
            .shared_codex_appserver
            .handle_thread_started_notification_for_test("T-empty")
            .await
            .unwrap()
    );
    assert_projected_codex_thread(&boot, card.id.as_str(), "T-empty", "started").await;
}

#[tokio::test]
async fn terminal_create_recovery_spawn_failure_clears_stale_pid_before_compensation() {
    let mut boot = boot_with_counted_spawn().await;
    let card_id = new_id();
    let runtime_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                None,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "terminal_id": term.id,
        "program": "/bin/sh",
        "cwd": "/tmp",
        "env": {},
    });
    boot.repo
        .terminal_set_pid(&term.id, Some(12_345))
        .await
        .unwrap();
    boot.repo
        .terminal_set_exit(&term.id, Some(-1), false)
        .await
        .unwrap();

    let cleared_before_failure = Arc::new(AtomicBool::new(false));
    let spawn_attempts = Arc::new(AtomicUsize::new(0));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let repo_for_hook = route_repo.clone();
    let expected_terminal_id = term.id.clone();
    let cleared_for_hook = cleared_before_failure.clone();
    let attempts_for_hook = spawn_attempts.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let repo = repo_for_hook.clone();
            let expected_terminal_id = expected_terminal_id.clone();
            let cleared = cleared_for_hook.clone();
            let attempts = attempts_for_hook.clone();
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                assert_eq!(terminal_id, expected_terminal_id);
                let term = repo
                    .terminal_get(&terminal_id)
                    .await?
                    .expect("terminal exists before compensation");
                assert_eq!(
                    term.pid, None,
                    "stale pid must be cleared before spawn hook runs"
                );
                assert_eq!(term.exit_code, None);
                assert!(!term.signal_killed);
                cleared.store(true, Ordering::SeqCst);
                Err(CalmError::Internal("forced spawn failure".into()))
            })
        },
    );
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let adapter = Arc::new(TerminalAdapter::new_with_spawn_hook(
        route_repo.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![adapter],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.with_operation_runtime(runtime);

    let op_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("recovery-fail-op")
    .bind("recovery-fail-key")
    .bind("recovery-fail-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&terminal_payload(&boot.wave_id)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::TxCommitted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    assert!(cleared_before_failure.load(Ordering::SeqCst));
    assert_eq!(spawn_attempts.load(Ordering::SeqCst), 1);
    let row = sqlx::query("SELECT phase, last_error FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let phase: String = row.try_get("phase").unwrap();
    let last_error: String = row.try_get("last_error").unwrap();
    assert_eq!(phase, "failed");
    assert!(last_error.contains("forced spawn failure"));
    assert!(boot.repo.terminal_get(&term.id).await.unwrap().is_none());
}

#[tokio::test]
async fn worker_recovery_skips_respawn_when_terminal_already_exited() {
    let mut boot = boot_with_counted_spawn().await;
    let spawn_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_hook = spawn_attempts.clone();
    let hook = Arc::new(
        move |_terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let attempts = attempts_for_hook.clone();
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(CalmError::Internal("unexpected worker respawn".into()))
            })
        },
    );
    let adapter = install_terminal_worker_runtime_with_hook(&mut boot, hook);

    let idem = "terminal-worker-recovery-exited";
    let payload = terminal_worker_payload(&boot.wave_id, idem);
    let op = pending_operation("terminal-worker", &boot.wave_id, payload.clone());
    insert_pending_operation_row(&boot.repo, &op).await;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    mark_operation_succeeded(&boot.repo, &op.id).await;
    let terminal_id = output.data["terminal_id"].as_str().unwrap().to_string();
    boot.repo
        .terminal_set_exit(&terminal_id, Some(0), false)
        .await
        .unwrap();

    let op_id = new_id();
    let now = now_ms();
    let target_type = output.target_type.clone();
    let target_id = output.target_id.clone();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'terminal-worker', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'spawn_started', ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("terminal-worker-recovery-exited-op")
    .bind(idem)
    .bind("terminal-worker-recovery-exited-hash")
    .bind(&target_type)
    .bind(target_id.as_deref())
    .bind(serde_json::to_string(&json!({ "type": target_type, "id": target_id })).unwrap())
    .bind(serde_json::to_string(&payload).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let mut rx = boot.state.events.subscribe();
    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::SpawnStarted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    assert_eq!(
        spawn_attempts.load(Ordering::SeqCst),
        0,
        "recorded terminal exit must suppress recovery respawn"
    );
    assert_terminal_worker_succeeded(&boot, &op_id).await;
    let term = boot
        .repo
        .terminal_get(&terminal_id)
        .await
        .unwrap()
        .expect("terminal row preserved");
    assert_eq!(term.exit_code, Some(0));
    assert!(!term.signal_killed);
    assert_eq!(event_kind_count(&boot.repo, "card.added").await, 1);
    let (added, failed) = drain_worker_event_counts(&mut rx, idem).await;
    assert_eq!(added, 1, "subscribers must see exactly one CardAdded");
    assert_eq!(failed, 0, "recovery preservation must not emit TaskFailed");

    let mut boot = boot_with_counted_spawn().await;
    let git_repo = init_git_repo_for_wave(&boot, "codex-worker-recovery-exited").await;
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let codex_adapter = Arc::new(CodexWorkerAdapter::new(
        route_repo.clone(),
        boot.state.codex.clone(),
        SharedCodexAppServer::new_stub(boot.repo.clone()),
        None,
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
    ));
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![codex_adapter.clone()],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.clone().with_operation_runtime(runtime);

    let idem = "codex-worker-recovery-exited";
    let payload = codex_worker_payload(&boot.wave_id, idem);
    let op = pending_operation("codex-worker", &boot.wave_id, payload.clone());
    insert_pending_operation_row(&boot.repo, &op).await;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let output = codex_adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(git_repo.join(".git").is_dir());
    mark_operation_succeeded(&boot.repo, &op.id).await;
    let terminal_id = output.data["terminal_id"].as_str().unwrap().to_string();
    boot.repo
        .terminal_set_exit(&terminal_id, Some(0), false)
        .await
        .unwrap();

    let op_id = new_id();
    let now = now_ms();
    let target_type = output.target_type.clone();
    let target_id = output.target_id.clone();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'codex-worker', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'spawn_started', ?10, ?11, ?12, ?12)"#,
    )
    .bind(&op_id)
    .bind("codex-worker-recovery-exited-op")
    .bind(idem)
    .bind("codex-worker-recovery-exited-hash")
    .bind(&target_type)
    .bind(target_id.as_deref())
    .bind(serde_json::to_string(&json!({ "type": target_type, "id": target_id })).unwrap())
    .bind(serde_json::to_string(&payload).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let mut rx = boot.state.events.subscribe();
    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(
        &plan.items[0],
        RecoveryItem::Recover {
            op_id: planned,
            from_phase: Phase::SpawnStarted,
            ..
        } if planned == &op_id
    ));
    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    assert_terminal_worker_succeeded(&boot, &op_id).await;
    let term = boot
        .repo
        .terminal_get(&terminal_id)
        .await
        .unwrap()
        .expect("codex worker terminal row preserved");
    assert_eq!(term.exit_code, Some(0));
    assert!(!term.signal_killed);
    assert_eq!(event_kind_count(&boot.repo, "card.added").await, 1);
    let (added, failed) = drain_worker_event_counts(&mut rx, idem).await;
    assert_eq!(added, 1, "codex worker recovery must emit CardAdded");
    assert_eq!(failed, 0, "codex worker recovery must not emit TaskFailed");
}

#[tokio::test]
async fn worker_spawn_error_then_fast_exit_finalizes_as_success() {
    let mut boot = boot_with_counted_spawn().await;
    let spawn_attempts = Arc::new(AtomicUsize::new(0));
    let repo_for_hook = boot.repo.clone();
    let attempts_for_hook = spawn_attempts.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let repo = repo_for_hook.clone();
            let attempts = attempts_for_hook.clone();
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                repo.terminal_set_exit(&terminal_id, Some(0), false).await?;
                Err(CalmError::Internal(
                    "forced spawn failure after recorded exit".into(),
                ))
            })
        },
    );
    install_terminal_worker_runtime_with_hook(&mut boot, hook);

    let idem = "terminal-worker-spawn-error-fast-exit";
    let payload = terminal_worker_payload(&boot.wave_id, idem);
    let mut rx = boot.state.events.subscribe();
    let op_id = boot
        .state
        .operation_runtime
        .submit(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(idem.into()),
                payload_hash: "terminal-worker-spawn-error-fast-exit-hash".into(),
            },
            payload,
        )
        .await
        .unwrap();
    let outcome = boot
        .state
        .operation_runtime
        .wait(&op_id)
        .await
        .unwrap()
        .outcome;
    assert!(
        matches!(outcome, OperationOutcome::Succeeded { .. }),
        "spawn error with recorded exit must finalize as success: {outcome:?}"
    );

    assert_eq!(spawn_attempts.load(Ordering::SeqCst), 1);
    assert_terminal_worker_succeeded(&boot, &op_id).await;
    let output = operation_tx_output(&boot.repo, &op_id).await;
    let terminal_id = output.data["terminal_id"].as_str().unwrap();
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row preserved");
    assert_eq!(term.exit_code, Some(0));
    assert!(!term.signal_killed);
    assert_eq!(event_kind_count(&boot.repo, "card.added").await, 1);
    assert_eq!(event_kind_count(&boot.repo, "task.failed").await, 0);
    let (added, failed) = drain_worker_event_counts(&mut rx, idem).await;
    assert_eq!(added, 1, "fast-exit preservation must emit CardAdded");
    assert_eq!(failed, 0, "fast-exit preservation must not emit TaskFailed");
}

#[tokio::test]
async fn apply_recovery_continues_after_drive_error_between_items() {
    let mut boot = boot_with_counted_spawn().await;
    let bad_op_id = new_id();
    let valid_op_id = new_id();
    let now = now_ms();

    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'wave', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&bad_op_id)
    .bind("bad-recovery-op")
    .bind("bad-recovery-key")
    .bind("bad-recovery-hash")
    .bind(&boot.wave_id)
    .bind(serde_json::to_string(&json!({ "type": "wave", "id": boot.wave_id })).unwrap())
    .bind(serde_json::to_string(&terminal_payload(&boot.wave_id)).unwrap())
    .bind(Option::<String>::None)
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let card_id = new_id();
    let runtime_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                None,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    let mut output = TxOutput::new(
        "runtime",
        Some(runtime_id.clone()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
        "runtime_id": runtime_id,
        "terminal_id": term.id,
        "program": "/bin/sh",
        "cwd": "/tmp",
        "env": {},
    });
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               tx_output_json, phase, lease_owner, lease_until_ms, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'runtime', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&valid_op_id)
    .bind("valid-recovery-op")
    .bind("valid-recovery-key")
    .bind("valid-recovery-hash")
    .bind(output.target_id.as_deref())
    .bind(
        serde_json::to_string(&json!({ "type": "runtime", "id": output.target_id.as_deref() }))
            .unwrap(),
    )
    .bind(serde_json::to_string(&terminal_payload(&boot.wave_id)).unwrap())
    .bind(serde_json::to_string(&output).unwrap())
    .bind("dead-process")
    .bind(now + 60_000)
    .bind(now + 1)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let repo = Arc::new(DriveErrorOnceRepo::new(SqlxOperationRepo::new(
        boot.repo.pool().clone(),
    )));
    let repo_for_runtime: Arc<dyn OperationRepo> = repo.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let count_for_hook = boot.spawn_count.clone();
    let hook = Arc::new(
        move |terminal_id: String,
              _program: String,
              _cwd: String,
              _env: Value|
              -> BoxFuture<'static, calm_server::error::Result<SpawnHandle>> {
            let count = count_for_hook.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    );
    let adapter = Arc::new(TerminalAdapter::new_with_spawn_hook(
        route_repo.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
        hook,
    ));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        repo_for_runtime.clone(),
        vec![adapter],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            repo_for_runtime,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.with_operation_runtime(runtime);

    let plan = boot
        .state
        .operation_runtime
        .recover_on_boot()
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 2);
    assert!(matches!(
        &plan.items[..],
        [
            RecoveryItem::Recover { op_id: bad, .. },
            RecoveryItem::Recover { op_id: valid, .. },
        ] if bad == &bad_op_id && valid == &valid_op_id
    ));

    boot.state
        .operation_runtime
        .apply_recovery(plan)
        .await
        .unwrap();

    let bad = sqlx::query("SELECT phase, last_error FROM operations WHERE id = ?1")
        .bind(&bad_op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let bad_phase: String = bad.try_get("phase").unwrap();
    let bad_error: String = bad.try_get("last_error").unwrap();
    assert_eq!(bad_phase, "stuck");
    assert!(bad_error.contains("missing tx_output_json"));

    let valid = sqlx::query("SELECT phase FROM operations WHERE id = ?1")
        .bind(&valid_op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    let valid_phase: String = valid.try_get("phase").unwrap();
    assert_eq!(valid_phase, "succeeded");
    assert_eq!(repo.drive_failures(), 1);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pre_pr4_operation_payload_deserializes_with_missing_runtime_id() {
    let terminal: TerminalCreateOperationPayload =
        serde_json::from_value(without_runtime_id(terminal_payload("wave-pre-pr4")))
            .expect("terminal payload without runtime_id");
    assert!(terminal.runtime_id.is_none());

    let codex: CodexCreateOperationPayload =
        serde_json::from_value(without_runtime_id(codex_payload("wave-pre-pr4", None)))
            .expect("codex payload without runtime_id");
    assert!(codex.runtime_id.is_none());

    let boot = boot_claude_with_counted_spawn().await;
    let claude: ClaudeCreateOperationPayload = serde_json::from_value(without_runtime_id(
        claude_payload(&boot, "wave-pre-pr4", None),
    ))
    .expect("claude payload without runtime_id");
    assert!(claude.runtime_id.is_none());
}

#[tokio::test]
async fn adapter_mints_runtime_id_when_payload_runtime_id_is_none() {
    let boot = boot_with_counted_spawn().await;
    let payload = without_runtime_id(terminal_payload(&boot.wave_id));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let adapter = TerminalAdapter::new(
        route_repo,
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
    );
    let op = pending_operation("terminal-create", &boot.wave_id, payload.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    assert_minted_runtime(&boot.repo, output, WorkerSessionKind::Terminal).await;

    let boot = boot_codex_with_counted_spawn().await;
    let payload = without_runtime_id(codex_payload(&boot.wave_id, None));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let adapter = CodexAdapter::new(
        route_repo,
        boot.state.codex.clone(),
        boot.state.shared_codex_appserver.clone(),
        boot.state.pending_codex_threads.clone(),
        boot.state.pending_codex_threads_spawn_serial.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
    );
    let op = pending_operation("codex-create", &boot.wave_id, payload.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    assert_minted_runtime(&boot.repo, output, WorkerSessionKind::CodexCard).await;

    let boot = boot_claude_with_counted_spawn().await;
    let payload = without_runtime_id(claude_payload(&boot, &boot.wave_id, None));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let adapter = ClaudeAdapter::new(
        route_repo,
        boot.state.codex.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
    );
    let op = pending_operation("claude-create", &boot.wave_id, payload.clone());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    assert_minted_runtime(&boot.repo, output, WorkerSessionKind::ClaudeCard).await;
}

async fn init_git_repo_for_wave(boot: &Boot, name: &str) -> PathBuf {
    let repo_path = boot._tmp.path().join(name);
    std::fs::create_dir_all(&repo_path).expect("create git repo dir");
    run_git(&repo_path, ["init"]);
    run_git(
        &repo_path,
        ["config", "user.email", "no-double-spawn@example.test"],
    );
    run_git(&repo_path, ["config", "user.name", "No Double Spawn Test"]);
    std::fs::write(repo_path.join("README.md"), "initial\n").expect("write readme");
    run_git(&repo_path, ["add", "README.md"]);
    run_git(&repo_path, ["commit", "-m", "initial"]);
    sqlx::query("UPDATE waves SET cwd = ?1 WHERE id = ?2")
        .bind(repo_path.to_string_lossy().as_ref())
        .bind(&boot.wave_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    repo_path
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

fn terminal_payload(wave_id: &str) -> Value {
    serde_json::to_value(TerminalCreateOperationPayload {
        actor: ActorId::User,
        runtime_id: Some(new_id()),
        request: TerminalCreateRequestPayload {
            wave_id: wave_id.to_string(),
            sort: Some(1.0),
            program: "/bin/sh".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        },
    })
    .unwrap()
}

fn terminal_worker_payload(wave_id: &str, idempotency_key: &str) -> Value {
    serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::User,
        wave_id: wave_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        cmd: "printf done\n".into(),
        cwd: Some("/tmp".into()),
    })
    .unwrap()
}

fn codex_worker_payload(wave_id: &str, idempotency_key: &str) -> Value {
    serde_json::to_value(CodexWorkerOperationPayload {
        actor: ActorId::User,
        wave_id: wave_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        goal: "recover completed codex worker".into(),
        cwd: None,
        context: json!({ "from": "no-double-spawn" }),
        acceptance_criteria: None,
    })
    .unwrap()
}

fn install_terminal_worker_runtime_with_hook(
    boot: &mut Boot,
    hook: TestTerminalWorkerSpawnHook,
) -> Arc<TerminalWorkerAdapter> {
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let adapter = Arc::new(TerminalWorkerAdapter::new_with_spawn_hook(
        route_repo.clone(),
        boot.state.card_role_cache.clone(),
        boot.state.wave_cove_cache.clone(),
        hook,
    ));
    let operation_repo = Arc::new(SqlxOperationRepo::new(boot.repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![adapter.clone()],
        boot.state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
            completion,
        ),
    ));
    boot.state = boot.state.clone().with_operation_runtime(runtime);
    adapter
}

fn codex_payload(wave_id: &str, prompt: Option<&str>) -> Value {
    serde_json::to_value(CodexCreateOperationPayload {
        actor: ActorId::User,
        runtime_id: Some(new_id()),
        request: NormalizedCodexCreateRequest {
            wave_id: wave_id.to_string(),
            sort: Some(1.0),
            cwd: "/workspace".into(),
            prompt: prompt.map(ToOwned::to_owned),
            icon_bg: None,
            icon_fg: None,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        },
    })
    .unwrap()
}

fn claude_payload(boot: &Boot, wave_id: &str, prompt: Option<&str>) -> Value {
    let card_id = new_id();
    let claude_session_id = uuid::Uuid::new_v4().to_string();
    let settings_path = boot
        .state
        .codex
        .claude_settings_dir
        .join(&card_id)
        .join("settings.json")
        .to_string_lossy()
        .to_string();
    let mut command_line = format!(
        "/bin/true --settings {} --session-id {}",
        settings_path, claude_session_id
    );
    if let Some(prompt) = prompt {
        command_line.push_str(" -- ");
        command_line.push_str(prompt);
    }
    serde_json::to_value(ClaudeCreateOperationPayload {
        actor: ActorId::User,
        runtime_id: Some(new_id()),
        request: PreparedClaudeCreateRequest {
            wave_id: wave_id.to_string(),
            sort: Some(1.0),
            cwd: "/workspace".into(),
            prompt: prompt.map(ToOwned::to_owned),
            icon_bg: None,
            icon_fg: None,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
            card_id: card_id.clone(),
            claude_session_id,
            settings_path,
            command_line,
            env: json!({
                "NEIGE_CARD_ID": card_id,
                "NEIGE_CALM_BASE_URL": boot.state.codex.ingest_url,
                "NEIGE_HOOK_PROVIDER": "claude",
            }),
        },
    })
    .unwrap()
}

fn without_runtime_id(mut payload: Value) -> Value {
    payload
        .as_object_mut()
        .expect("operation payload object")
        .remove("runtime_id")
        .expect("payload includes runtime_id");
    payload
}

fn pending_operation(kind: &str, target_id: &str, payload: Value) -> Operation {
    Operation {
        id: new_id(),
        operation_key: new_id(),
        kind: kind.into(),
        idempotency_key: None,
        payload_hash: "payload-hash".into(),
        target_type: "wave".into(),
        target_id: Some(target_id.into()),
        target: json!({ "type": "wave", "id": target_id }),
        payload,
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

async fn insert_pending_operation_row(repo: &SqlxRepo, op: &Operation) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO operations (
               id, operation_key, kind, idempotency_key, payload_hash,
               target_type, target_id, target_json, payload_json,
               phase, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)"#,
    )
    .bind(&op.id)
    .bind(&op.operation_key)
    .bind(&op.kind)
    .bind(&op.idempotency_key)
    .bind(&op.payload_hash)
    .bind(&op.target_type)
    .bind(op.target_id.as_deref())
    .bind(serde_json::to_string(&op.target).unwrap())
    .bind(serde_json::to_string(&op.payload).unwrap())
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn mark_operation_succeeded(repo: &SqlxRepo, op_id: &str) {
    let now = now_ms();
    sqlx::query(
        r#"UPDATE operations
              SET phase = 'succeeded',
                  updated_at_ms = ?2,
                  completed_at_ms = ?2
            WHERE id = ?1"#,
    )
    .bind(op_id)
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn assert_minted_runtime(repo: &SqlxRepo, output: TxOutput, kind: WorkerSessionKind) {
    let runtime_id = output
        .target_id
        .as_deref()
        .expect("runtime target id")
        .to_string();
    assert!(!runtime_id.is_empty());
    assert_eq!(output.target_type, "runtime");
    assert_eq!(
        output.data["runtime_id"].as_str(),
        Some(runtime_id.as_str())
    );

    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .expect("minted runtime row");
    assert_eq!(runtime.id, runtime_id);
    assert_eq!(runtime.kind, kind);
}

async fn assert_terminal_worker_succeeded(boot: &Boot, op_id: &str) {
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(phase, "succeeded");
}

async fn operation_tx_output(repo: &SqlxRepo, op_id: &str) -> TxOutput {
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

async fn event_kind_count(repo: &SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn drain_worker_event_counts(
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
    idempotency_key: &str,
) -> (usize, usize) {
    let mut added = 0;
    let mut failed = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(25), rx.recv()).await {
            Ok(Ok(env)) => match env.event {
                Event::CardAdded(card)
                    if card.payload.get("idempotency_key").and_then(Value::as_str)
                        == Some(idempotency_key) =>
                {
                    added += 1;
                }
                Event::TaskFailed {
                    idempotency_key: failed_key,
                    ..
                } if failed_key == idempotency_key => {
                    failed += 1;
                }
                _ => {}
            },
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => {}
        }
    }
    (added, failed)
}

async fn wait_for_notification_receivers(shared: &SharedCodexAppServer, min: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if shared.notification_receiver_count_for_test() >= min {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for shared codex notification receiver"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_for_terminal_phase(boot: &Boot, op_id: &str, timeout: Duration) -> PhaseTag {
    let repo = SqlxOperationRepo::new(boot.repo.pool().clone());
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        boot.state.operation_runtime.drive().await.unwrap();
        let op = repo
            .get_operation(op_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("operation {op_id} missing"));
        let phase = op.phase.tag();
        if is_terminal_phase(phase) {
            return phase;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for terminal operation phase; op_id={op_id}, phase={}",
            phase.as_str()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn is_terminal_phase(phase: PhaseTag) -> bool {
    matches!(
        phase,
        PhaseTag::Succeeded | PhaseTag::Failed | PhaseTag::Stuck
    )
}

fn output_optional_i64_for_test(output: &TxOutput, key: &str) -> Option<i64> {
    output.data.get(key).and_then(Value::as_i64)
}

async fn seed_codex_card_for_operation(
    boot: &Boot,
    card_id: String,
    prompt: Option<&str>,
) -> (Card, String, Value, String) {
    let wave_id = boot.wave_id.clone();
    let runtime_id = new_id();
    let cache = boot.state.card_role_cache.clone();
    let env = json!({
        "CODEX_HOME": boot.state.codex.codex_home_dir().to_string_lossy().to_string(),
        "NEIGE_CARD_ID": card_id.clone(),
        "NEIGE_CALM_BASE_URL": boot.state.codex.ingest_url,
    });
    let env_for_output = env.clone();
    let prompt_for_tx = prompt.map(ToOwned::to_owned);
    let runtime_id_for_tx = runtime_id.clone();
    let (card, term, _token) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_codex_create_tx(
                tx,
                card_id,
                &runtime_id_for_tx,
                None,
                wave_id.into(),
                None,
                "/workspace".into(),
                env,
                prompt_for_tx,
                None,
                None,
                CardRole::Worker,
                true,
                &cache,
                calm_server::routes::theme::RequestTheme::default_dark(),
            )
            .await
        })
    })
    .await
    .unwrap();
    (card, term.id.to_string(), env_for_output, runtime_id)
}

fn terminal_route_body() -> Value {
    json!({
        "program": "/bin/sh",
        "cwd": "/tmp",
        "env": {},
        "sort": 1.0,
        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
    })
}

async fn post_terminal_card_route(
    app: axum::Router,
    wave_id: &str,
    body: Value,
    idempotency_key: Option<&str>,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/waves/{wave_id}/terminal-cards"))
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    if let Some(actor) = actor {
        req = req.header("X-Calm-Actor", actor);
    }
    let resp = app
        .oneshot(
            req.body(Body::from(body.to_string()))
                .expect("build terminal-card POST request"),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn result_card_id(outcome: &OperationOutcome) -> String {
    let value = match outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => result,
        other => panic!("expected success, got {other:?}"),
    };
    let card: Card = serde_json::from_value(value.clone()).unwrap();
    card.id.to_string()
}

fn response_card_id(body: &Value) -> String {
    let card: Card = serde_json::from_value(body.clone()).unwrap();
    card.id.to_string()
}
