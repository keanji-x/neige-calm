#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_terminal_create_tx};
use calm_server::db::write_in_tx_typed;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{BroadcastEnvelope, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{Card, CardRole, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::terminal_adapter::{
    TerminalAdapter, TerminalCreateOperationPayload, TerminalCreateRequestPayload,
};
use calm_server::operation::{
    CompensationStateVersioned, Operation, OperationKey, OperationOutcome, OperationRepo,
    OperationResult, OperationRuntime, Phase, ProviderAdapter, RecoveryItem, SpawnCtx, SpawnHandle,
    SqlxOperationRepo, TxOutput,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
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

#[async_trait]
impl OperationRepo for DriveErrorOnceRepo {
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

    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> CalmResult<Option<Operation>> {
        self.inner.set_compensating(op, state).await
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
                Ok(SpawnHandle {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
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
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![terminal_adapter],
        events.clone(),
        SpawnCtx::new(
            route_repo,
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            events,
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
    assert!(
        !boot
            .state
            .dispatcher
            .recently_seen_contains("terminal-create-same-key"),
        "OperationRuntime terminal-create must not install dispatcher recently_seen"
    );
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
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Plain,
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
        "card",
        Some(card.id.to_string()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
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
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'wave', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("recovery-op")
    .bind("recovery-key")
    .bind("recovery-hash")
    .bind(&boot.wave_id)
    .bind(serde_json::to_string(&json!({ "type": "wave", "id": boot.wave_id })).unwrap())
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
async fn terminal_create_recovery_spawn_failure_clears_stale_pid_before_compensation() {
    let mut boot = boot_with_counted_spawn().await;
    let card_id = new_id();
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Plain,
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
        "card",
        Some(card.id.to_string()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
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
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![adapter],
        boot.state.events.clone(),
        SpawnCtx::new(
            route_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
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
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'wave', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&op_id)
    .bind("recovery-fail-op")
    .bind("recovery-fail-key")
    .bind("recovery-fail-hash")
    .bind(&boot.wave_id)
    .bind(serde_json::to_string(&json!({ "type": "wave", "id": boot.wave_id })).unwrap())
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
    let wave_id = boot.wave_id.clone();
    let cache = boot.state.card_role_cache.clone();
    let (card, term) = write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            card_with_terminal_create_tx(
                tx,
                card_id,
                wave_id.into(),
                None,
                "/bin/sh".into(),
                "/tmp".into(),
                json!({}),
                CardRole::Plain,
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
        "card",
        Some(card.id.to_string()),
        serde_json::to_value(&card).unwrap(),
    );
    output.data = json!({
        "card_id": card.id,
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
           VALUES (?1, ?2, 'terminal-create', ?3, ?4, 'wave', ?5, ?6, ?7, ?8, 'tx_committed', ?9, ?10, ?11, ?11)"#,
    )
    .bind(&valid_op_id)
    .bind("valid-recovery-op")
    .bind("valid-recovery-key")
    .bind("valid-recovery-hash")
    .bind(&boot.wave_id)
    .bind(serde_json::to_string(&json!({ "type": "wave", "id": boot.wave_id })).unwrap())
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
                Ok(SpawnHandle {
                    renderer_id: terminal_id.clone(),
                    terminal_id,
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
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        repo_for_runtime,
        vec![adapter],
        boot.state.events.clone(),
        SpawnCtx::new(
            route_repo,
            boot.state.daemon.clone(),
            boot.state.terminal_renderer.clone(),
            boot.state.events.clone(),
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

fn terminal_payload(wave_id: &str) -> Value {
    serde_json::to_value(TerminalCreateOperationPayload {
        actor: ActorId::User,
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
