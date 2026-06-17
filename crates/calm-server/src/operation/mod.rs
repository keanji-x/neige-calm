#[cfg(test)]
mod parked_fence_model;

pub mod claude_adapter;
pub mod claude_restart_adapter;
pub mod codex_adapter;
pub mod spec_harness_interrupt_adapter;
pub mod spec_harness_shutdown_adapter;
pub mod spec_harness_start_adapter;
pub mod task_verify_adapter;
pub mod terminal_adapter;
pub(crate) mod worker_cleanup;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tokio::sync::{Mutex, broadcast};

use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, EventBus};
use crate::model::{new_id, now_ms};
use crate::proc_identity::{signal_process_group, verify_owned_pid};
use crate::routes::terminal::spawn_terminal_with_parts;
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

pub type OperationId = String;
pub type TimestampMs = i64;
pub type Tx<'tx> = Transaction<'tx, Sqlite>;
const OPERATION_LEASE_MS: TimestampMs = 60_000;

#[derive(Clone, Copy)]
enum ParkedClaimMode {
    SteadyState,
    Boot,
}

#[derive(Clone, Debug)]
pub struct OperationKey {
    pub operation_key: String,
    pub idempotency_key: Option<String>,
    pub payload_hash: String,
}

/// Fragment shared by every `(kind, idempotency_key)` payload-hash
/// conflict message — kept in one place so
/// [`is_idempotency_payload_conflict`] can match reliably.
const IDEMPOTENCY_PAYLOAD_CONFLICT_MSG: &str = "already used with different payload";

/// The submit/insert idempotency conflict: the `(kind, idempotency_key)`
/// pair already exists with a DIFFERENT payload hash. Built in one place
/// (used by both [`OperationRuntime::submit`] and the repo's
/// `insert_operation`) so callers can classify it via
/// [`is_idempotency_payload_conflict`].
fn idempotency_payload_conflict(idempotency_key: Option<&str>) -> CalmError {
    let key = idempotency_key.unwrap_or("<missing idempotency key>");
    CalmError::Conflict(format!(
        "operation idempotency key {key} {IDEMPOTENCY_PAYLOAD_CONFLICT_MSG}"
    ))
}

/// True iff `e` is the [`idempotency_payload_conflict`] error — the key
/// is already bound to an operation with a different payload hash. The
/// scheduler classifies this as a PERMANENT spawn error (round-3 review
/// F1): its payloads are pure functions of the frozen task row, so the
/// mismatch can only be a foreign/legacy operation owning the key, and
/// retrying can never self-heal.
pub fn is_idempotency_payload_conflict(e: &CalmError) -> bool {
    matches!(e, CalmError::Conflict(msg) if msg.contains(IDEMPOTENCY_PAYLOAD_CONFLICT_MSG))
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub id: OperationId,
    pub operation_key: String,
    pub kind: String,
    pub idempotency_key: Option<String>,
    pub payload_hash: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub target: Value,
    pub payload: Value,
    pub tx_output: Option<TxOutput>,
    pub phase: Phase,
    pub phase_detail: Option<Value>,
    pub attempt: i32,
    pub last_error: Option<String>,
    pub compensation_state: Option<Value>,
    pub lease_owner: Option<String>,
    pub lease_until_ms: Option<TimestampMs>,
    pub spawn_artifacts: Option<SpawnArtifacts>,
    pub parked_at_ms: Option<TimestampMs>,
    pub parked_deadline_ms: Option<TimestampMs>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxOutput {
    pub target_type: String,
    pub target_id: Option<String>,
    pub result: Value,
    #[serde(default)]
    pub data: Value,
    #[serde(skip)]
    pub post_commit_events: Vec<BroadcastEnvelope>,
}

impl TxOutput {
    pub fn new(target_type: impl Into<String>, target_id: Option<String>, result: Value) -> Self {
        Self {
            target_type: target_type.into(),
            target_id,
            result,
            data: Value::Null,
            post_commit_events: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct SpawnCtx {
    pub repo: Arc<dyn crate::db::RouteRepo>,
    pub operation_repo: Arc<dyn OperationRepo>,
    pub daemon: Arc<DaemonClient>,
    pub terminal_renderer: Arc<TerminalRendererRegistry>,
    pub events: EventBus,
    pub completion: OperationCompletionBus,
}

impl SpawnCtx {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        operation_repo: Arc<dyn OperationRepo>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        events: EventBus,
        completion: OperationCompletionBus,
    ) -> Self {
        Self {
            repo,
            operation_repo,
            daemon,
            terminal_renderer,
            events,
            completion,
        }
    }

    pub async fn spawn_terminal(
        &self,
        term: &crate::model::Terminal,
        program: &str,
        cwd: &str,
        env: &Value,
    ) -> Result<SpawnHandle> {
        let entry = spawn_terminal_with_parts(
            self.daemon.as_ref(),
            self.terminal_renderer.as_ref(),
            self.repo.as_ref(),
            term,
            program,
            cwd,
            env,
        )
        .await?;
        Ok(SpawnHandle::Terminal {
            terminal_id: term.id.clone(),
            renderer_id: entry.terminal_id.clone(),
        })
    }

    pub async fn record_spawn_artifacts(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
    ) -> Result<()> {
        self.operation_repo
            .record_spawn_artifacts(op, artifacts)
            .await
    }
}

// #679 PR1 — `SpawnHandle` moved to `calm_exec::provider` (it is part of
// the execution contract: `WorkerProvider::resume` returns it). Re-exported
// so every `crate::operation::SpawnHandle` path is unchanged.
pub use calm_exec::SpawnHandle;

pub type ParkedObserver = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub enum SpawnOutcome {
    Ready(SpawnHandle),
    Parked {
        deadline_ms: TimestampMs,
        observer: ParkedObserver,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnArtifacts {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: u64,
    pub boot_id: String,
    pub log_path: Option<String>,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Clone, Debug)]
pub enum AppServerInteractOutcome {
    NotApplicable,
    MintedAndAwaited { thread_id: String },
    RegisteredPendingForLaterAttribution { entry_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Pending,
    TxCommitted,
    AppServerInteract { kind: AppServerInteractKind },
    SpawnStarted,
    SpawnSucceeded,
    Parked,
    Succeeded,
    Compensating,
    Failed,
    Stuck { reason: String, since: TimestampMs },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppServerInteractKind {
    MintAndAwait { thread_id: Option<String> },
    RegisterPending { entry_id: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseTag {
    Pending,
    TxCommitted,
    AppServerInteract,
    SpawnStarted,
    SpawnSucceeded,
    Parked,
    Succeeded,
    Compensating,
    Failed,
    Stuck,
}

impl PhaseTag {
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseTag::Pending => "pending",
            PhaseTag::TxCommitted => "tx_committed",
            PhaseTag::AppServerInteract => "app_server_interact",
            PhaseTag::SpawnStarted => "spawn_started",
            PhaseTag::SpawnSucceeded => "spawn_succeeded",
            PhaseTag::Parked => "parked",
            PhaseTag::Succeeded => "succeeded",
            PhaseTag::Compensating => "compensating",
            PhaseTag::Failed => "failed",
            PhaseTag::Stuck => "stuck",
        }
    }

    pub fn from_db_str(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "tx_committed" => Ok(Self::TxCommitted),
            "app_server_interact" => Ok(Self::AppServerInteract),
            "spawn_started" => Ok(Self::SpawnStarted),
            "spawn_succeeded" => Ok(Self::SpawnSucceeded),
            "parked" => Ok(Self::Parked),
            "succeeded" => Ok(Self::Succeeded),
            "compensating" => Ok(Self::Compensating),
            "failed" => Ok(Self::Failed),
            "stuck" => Ok(Self::Stuck),
            other => Err(CalmError::Internal(format!(
                "unknown operation phase {other}"
            ))),
        }
    }
}

impl Phase {
    pub fn tag(&self) -> PhaseTag {
        match self {
            Phase::Pending => PhaseTag::Pending,
            Phase::TxCommitted => PhaseTag::TxCommitted,
            Phase::AppServerInteract { .. } => PhaseTag::AppServerInteract,
            Phase::SpawnStarted => PhaseTag::SpawnStarted,
            Phase::SpawnSucceeded => PhaseTag::SpawnSucceeded,
            Phase::Parked => PhaseTag::Parked,
            Phase::Succeeded => PhaseTag::Succeeded,
            Phase::Compensating => PhaseTag::Compensating,
            Phase::Failed => PhaseTag::Failed,
            Phase::Stuck { .. } => PhaseTag::Stuck,
        }
    }

    pub fn serialize_split(&self) -> (PhaseTag, Option<Value>) {
        match self {
            Phase::AppServerInteract { kind } => {
                let detail = match kind {
                    AppServerInteractKind::MintAndAwait { thread_id } => json!({
                        "kind": "mint_and_await",
                        "thread_id": thread_id,
                    }),
                    AppServerInteractKind::RegisterPending { entry_id } => json!({
                        "kind": "register_pending",
                        "entry_id": entry_id,
                    }),
                };
                (PhaseTag::AppServerInteract, Some(detail))
            }
            Phase::Stuck { reason, since } => (
                PhaseTag::Stuck,
                Some(json!({
                    "reason": reason,
                    "since": since,
                })),
            ),
            _ => (self.tag(), None),
        }
    }

    pub fn deserialize_join(disc: &str, detail: Option<&Value>) -> Result<Self> {
        match PhaseTag::from_db_str(disc)? {
            PhaseTag::Pending => Ok(Self::Pending),
            PhaseTag::TxCommitted => Ok(Self::TxCommitted),
            PhaseTag::AppServerInteract => {
                let detail = detail.ok_or_else(|| {
                    CalmError::Internal("app_server_interact missing phase detail".into())
                })?;
                let kind = detail.get("kind").and_then(Value::as_str).ok_or_else(|| {
                    CalmError::Internal("app_server_interact missing kind".into())
                })?;
                match kind {
                    "mint_and_await" => Ok(Self::AppServerInteract {
                        kind: AppServerInteractKind::MintAndAwait {
                            thread_id: detail
                                .get("thread_id")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                        },
                    }),
                    "register_pending" => Ok(Self::AppServerInteract {
                        kind: AppServerInteractKind::RegisterPending {
                            entry_id: detail
                                .get("entry_id")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                        },
                    }),
                    other => Err(CalmError::Internal(format!(
                        "unknown app_server_interact kind {other}"
                    ))),
                }
            }
            PhaseTag::SpawnStarted => Ok(Self::SpawnStarted),
            PhaseTag::SpawnSucceeded => Ok(Self::SpawnSucceeded),
            PhaseTag::Parked => Ok(Self::Parked),
            PhaseTag::Succeeded => Ok(Self::Succeeded),
            PhaseTag::Compensating => Ok(Self::Compensating),
            PhaseTag::Failed => Ok(Self::Failed),
            PhaseTag::Stuck => {
                let detail =
                    detail.ok_or_else(|| CalmError::Internal("stuck missing detail".into()))?;
                let reason = detail
                    .get("reason")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CalmError::Internal("stuck missing reason".into()))?
                    .to_string();
                let since = detail
                    .get("since")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| CalmError::Internal("stuck missing since".into()))?;
                Ok(Self::Stuck { reason, since })
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationStep {
    pub op: String,
    pub args: Value,
    pub completed: bool,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationStateVersioned {
    pub version: u32,
    pub from_phase: PhaseTag,
    pub reason: String,
    pub steps: Vec<CompensationStep>,
}

#[derive(Clone, Debug)]
pub enum OperationOutcome {
    Succeeded {
        result: Value,
    },
    SucceededViaCollision {
        existing_op_id: OperationId,
        result: Value,
    },
    Failed {
        last_error: String,
        from_phase: PhaseTag,
        last_error_class: Option<String>,
    },
    Stuck {
        reason: String,
        from_phase: PhaseTag,
    },
}

#[derive(Clone, Debug)]
pub struct OperationResult {
    pub op_id: OperationId,
    pub outcome: OperationOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParkedOutcome {
    Succeeded {
        result: Value,
    },
    Failed {
        last_error: String,
        last_error_class: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum ParkedCompletion {
    Completed(OperationResult),
    AlreadyResolved { phase: PhaseTag },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParkedRecovery {
    LeaveParked,
    Complete(ParkedOutcome),
    Fail { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryMode {
    Boot,
    PreDeadlineProbe,
    PastDeadline,
}

#[derive(Clone, Debug)]
pub struct RecoveryPlan {
    pub items: Vec<RecoveryItem>,
}

#[derive(Clone, Debug)]
pub enum RecoveryItem {
    Recover {
        op_id: OperationId,
        from_phase: Phase,
        action: String,
    },
    Compensate {
        op_id: OperationId,
        reason: String,
    },
    VerifyParked {
        op_id: OperationId,
    },
    Skip {
        op_id: OperationId,
        reason: String,
    },
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> &'static str;
    fn phases(&self) -> &'static [PhaseTag];
    fn app_server_interact_kind(
        &self,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        Err(CalmError::Internal(format!(
            "{} does not declare an app_server_interact kind",
            self.kind()
        )))
    }

    async fn validate(&self, input: &Value) -> Result<()>;

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput>;

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome>;

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome>;

    async fn recover_parked(
        &self,
        _op: &Operation,
        _artifacts: &SpawnArtifacts,
        alive: bool,
        _mode: RecoveryMode,
        _ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        Ok(if alive {
            ParkedRecovery::LeaveParked
        } else {
            ParkedRecovery::Fail {
                reason: "parked process dead with no recorded outcome".into(),
            }
        })
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned>;

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()>;
}

#[async_trait]
pub trait OperationRepo: Send + Sync {
    fn sqlite_pool(&self) -> SqlitePool;
    async fn assert_sqlite_version(&self) -> Result<()>;
    async fn insert_operation(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId>;
    async fn find_by_idempotency_key(
        &self,
        kind: &str,
        key: &OperationKey,
    ) -> Result<Option<Operation>>;
    async fn get_operation(&self, op_id: &str) -> Result<Option<Operation>>;
    async fn operation_result(&self, op_id: &str) -> Result<Option<OperationResult>>;
    async fn claim_drive_batch(&self, limit: i64) -> Result<Vec<Operation>>;
    async fn abandoned_running_operations_on_boot(&self) -> Result<Vec<Operation>>;
    /// Reserved for PR2 background driver loop (design §B.3).
    async fn abandoned_running_operations_steady_state(&self) -> Result<Vec<Operation>>;
    async fn claim_operation_for_recovery(&self, op_id: &str) -> Result<Option<Operation>>;
    async fn record_spawn_artifacts(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
    ) -> Result<()> {
        let artifacts_text = serde_json::to_string(artifacts)?;
        let pool = self.sqlite_pool();
        let result = sqlx::query(
            r#"UPDATE operations
               SET spawn_artifacts_json = ?1,
                   updated_at_ms = ?2
               WHERE id = ?3
                 AND lease_owner = ?4
                 AND phase = 'spawn_started'"#,
        )
        .bind(artifacts_text)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(CalmError::Internal(format!(
                "operation {} lost lease while recording spawn artifacts",
                op.id
            )));
        }
        Ok(())
    }

    async fn set_parked(
        &self,
        op: &Operation,
        deadline_ms: TimestampMs,
    ) -> Result<Option<Operation>> {
        let now = now_ms();
        let pool = self.sqlite_pool();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'parked',
                   phase_detail_json = NULL,
                   parked_at_ms = ?1,
                   parked_deadline_ms = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   updated_at_ms = ?1
               WHERE id = ?3
                 AND lease_owner = ?4
                 AND spawn_artifacts_json IS NOT NULL"#,
        )
        .bind(now)
        .bind(deadline_ms)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_operation(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn claim_parked(&self, op_id: &str) -> Result<Option<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        let pool = self.sqlite_pool();
        let result = sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'
                 AND (lease_owner IS NULL OR lease_until_ms < ?3)"#,
        )
        .bind(&lease_owner)
        .bind(lease_until)
        .bind(now)
        .bind(op_id)
        .execute(&pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        fetch_claimed_parked(&pool, op_id, &lease_owner).await
    }

    async fn claim_parked_for_boot(&self, op_id: &str) -> Result<Option<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        let pool = self.sqlite_pool();
        let result = sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'"#,
        )
        .bind(&lease_owner)
        .bind(lease_until)
        .bind(now)
        .bind(op_id)
        .execute(&pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        fetch_claimed_parked(&pool, op_id, &lease_owner).await
    }

    async fn clear_parked_lease_for_boot(&self, op_id: &str) -> Result<()> {
        let pool = self.sqlite_pool();
        sqlx::query(
            r#"UPDATE operations
               SET lease_owner = NULL,
                   lease_until_ms = NULL,
                   updated_at_ms = ?1
               WHERE id = ?2
                 AND phase = 'parked'"#,
        )
        .bind(now_ms())
        .bind(op_id)
        .execute(&pool)
        .await?;
        Ok(())
    }

    async fn parked_operations(&self) -> Result<Vec<Operation>> {
        let pool = self.sqlite_pool();
        let rows = sqlx::query(
            r#"SELECT *
               FROM operations
               WHERE phase = 'parked'
               ORDER BY created_at_ms ASC"#,
        )
        .fetch_all(&pool)
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    async fn prepare_tx_and_advance(
        &self,
        op: &Operation,
        adapter: &dyn ProviderAdapter,
    ) -> Result<Option<(Operation, Vec<BroadcastEnvelope>)>>;
    async fn set_phase(&self, op: &Operation, phase: Phase) -> Result<Option<Operation>>;
    async fn set_phase_and_tx_output(
        &self,
        op: &Operation,
        phase: Phase,
        output: &TxOutput,
    ) -> Result<Option<Operation>>;
    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
        output: &TxOutput,
    ) -> Result<Option<Operation>>;
    async fn update_compensation_state(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> Result<Option<Operation>>;
    async fn mark_failed(
        &self,
        op: &Operation,
        last_error: String,
        from_phase: PhaseTag,
        last_error_class: Option<String>,
    ) -> Result<Option<OperationResult>>;
    async fn mark_stuck(
        &self,
        op: &Operation,
        reason: String,
        from_phase: PhaseTag,
    ) -> Result<Option<OperationResult>>;
}

#[derive(Clone)]
pub struct SqlxOperationRepo {
    pool: SqlitePool,
}

impl SqlxOperationRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn claim_operation_for_boot_recovery(&self, op_id: &str) -> Result<Option<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        let result = sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase IN (
                   'pending',
                   'tx_committed',
                   'app_server_interact',
                   'spawn_started',
                   'spawn_succeeded',
                   'compensating'
                 )"#,
        )
        .bind(&lease_owner)
        .bind(lease_until)
        .bind(now)
        .bind(op_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(op_id).await
    }
}

/// Completion fan-out uses a broadcast channel rather than a oneshot map.
/// That lets `wait()` first check the durable row, then subscribe without
/// losing a completion that raced just before the waiter arrived.
#[derive(Clone)]
pub struct OperationCompletionBus {
    tx: broadcast::Sender<OperationResult>,
}

impl OperationCompletionBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(128);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OperationResult> {
        self.tx.subscribe()
    }

    pub fn complete(&self, result: OperationResult) {
        let _ = self.tx.send(result);
    }
}

impl Default for OperationCompletionBus {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OperationRuntime {
    repo: Arc<dyn OperationRepo>,
    kinds: HashMap<&'static str, Arc<dyn ProviderAdapter>>,
    completion: OperationCompletionBus,
    events: EventBus,
    spawn_ctx: SpawnCtx,
    // PR2: replace with a singleton background driver loop per design §B.3.
    drive_mutex: Mutex<()>,
}

impl OperationRuntime {
    pub async fn new(
        repo: Arc<dyn OperationRepo>,
        kinds: Vec<Arc<dyn ProviderAdapter>>,
        events: EventBus,
        completion: OperationCompletionBus,
        spawn_ctx: SpawnCtx,
    ) -> Result<Self> {
        repo.assert_sqlite_version().await?;
        Ok(Self::new_unchecked(
            repo, kinds, events, completion, spawn_ctx,
        ))
    }

    pub fn new_unchecked(
        repo: Arc<dyn OperationRepo>,
        kinds: Vec<Arc<dyn ProviderAdapter>>,
        events: EventBus,
        completion: OperationCompletionBus,
        spawn_ctx: SpawnCtx,
    ) -> Self {
        let kinds = kinds
            .into_iter()
            .map(|adapter| (adapter.kind(), adapter))
            .collect();
        Self {
            repo,
            kinds,
            completion,
            events,
            spawn_ctx,
            drive_mutex: Mutex::new(()),
        }
    }

    pub fn publish_completion(&self, result: OperationResult) {
        self.completion.complete(result);
    }

    pub async fn submit(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        let adapter = self.adapter(kind)?;
        if let Some(existing) = self.repo.find_by_idempotency_key(kind, &key).await? {
            if existing.payload_hash == key.payload_hash {
                let op_id = existing.id;
                self.drive().await?;
                return Ok(op_id);
            }
            return Err(idempotency_payload_conflict(key.idempotency_key.as_deref()));
        }
        adapter.validate(&payload).await?;
        let op_id = self.repo.insert_operation(kind, key, payload).await?;
        self.drive().await?;
        Ok(op_id)
    }

    pub async fn start(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        self.submit(kind, key, payload).await
    }

    /// Issue #644 PR-B — look up an operation row by
    /// `(kind, idempotency_key)`. Used by the scheduler's sweep to
    /// correlate a `dispatched`/`running` task row with its worker-spawn
    /// operation (the task-to-operation relation is the idempotency-key
    /// convention, design §2.2; no `spawn_op_id` column exists).
    pub async fn find_by_kind_and_idempotency(
        &self,
        kind: &str,
        idempotency_key: &str,
    ) -> Result<Option<Operation>> {
        self.repo
            .find_by_idempotency_key(
                kind,
                &OperationKey {
                    operation_key: String::new(),
                    idempotency_key: Some(idempotency_key.to_string()),
                    payload_hash: String::new(),
                },
            )
            .await
    }

    pub async fn wait(&self, op_id: &OperationId) -> Result<OperationResult> {
        if let Some(result) = self.repo.operation_result(op_id).await? {
            return Ok(result);
        }
        let mut rx = self.completion.subscribe();
        loop {
            tokio::select! {
                received = rx.recv() => {
                    match received {
                        Ok(result) if result.op_id == *op_id => return Ok(result),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(CalmError::Internal("operation completion bus closed".into()));
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {
                    if let Some(result) = self.repo.operation_result(op_id).await? {
                        return Ok(result);
                    }
                    self.enforce_parked_deadline(op_id).await?;
                    self.drive().await?;
                }
            }
        }
    }

    pub async fn cancel_parked(&self, op_id: &OperationId, reason: &str) -> Result<bool> {
        let Some(op) = self.repo.claim_parked(op_id).await? else {
            return Ok(false);
        };
        let adapter = self.adapter(&op.kind)?;
        let output = required_output(&op)?.clone();
        let state = adapter
            .plan_compensation(PhaseTag::Parked, reason, &output, &op)
            .await?;
        if self
            .repo
            .set_compensating(&op, &state, &output)
            .await?
            .is_none()
        {
            log_lost_lease(&op, PhaseTag::Compensating);
            return Ok(false);
        }
        self.drive().await?;
        Ok(true)
    }

    pub async fn sweep_parked(&self) -> Result<()> {
        self.sweep_parked_with_claim(ParkedClaimMode::SteadyState)
            .await
    }

    async fn enforce_parked_deadline(&self, op_id: &OperationId) -> Result<()> {
        let Some(op) = self.repo.get_operation(op_id).await? else {
            return Ok(());
        };
        if !matches!(op.phase, Phase::Parked) {
            return Ok(());
        }
        self.apply_parked_sweep(op).await
    }

    pub async fn drive(&self) -> Result<()> {
        let _g = self.drive_mutex.lock().await;
        loop {
            let batch = self.repo.claim_drive_batch(32).await?;
            if batch.is_empty() {
                return Ok(());
            }
            for op in batch {
                let from_phase = op.phase.tag();
                let adapter = self.adapter(&op.kind)?;
                if let Err(e) = self.drive_one(adapter, op.clone()).await {
                    if let Some(result) = self
                        .repo
                        .mark_stuck(&op, format!("operation drive failed: {e}"), from_phase)
                        .await?
                    {
                        self.completion.complete(result);
                    } else {
                        log_lost_lease(&op, PhaseTag::Stuck);
                    }
                }
            }
        }
    }

    pub async fn recover_on_boot(&self) -> Result<RecoveryPlan> {
        let rows = self.repo.abandoned_running_operations_on_boot().await?;
        let mut items = Vec::new();
        for op in rows {
            let adapter = self.adapter(&op.kind)?;
            items.push(self.plan_recovery_for(adapter.as_ref(), &op).await?);
        }
        Ok(RecoveryPlan { items })
    }

    pub async fn apply_recovery(&self, plan: RecoveryPlan) -> Result<()> {
        for item in plan.items {
            match self.apply_recovery_item(item.clone()).await {
                Ok(()) => {
                    if let Err(e) = self.drive().await {
                        tracing::error!(
                            error = %e,
                            item = ?item,
                            "operation recovery drive failed; continuing"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        item = ?item,
                        "operation recovery item failed; continuing"
                    );
                }
            }
        }
        self.sweep_parked_for_boot().await?;
        Ok(())
    }

    fn adapter(&self, kind: &str) -> Result<Arc<dyn ProviderAdapter>> {
        self.kinds
            .get(kind)
            .cloned()
            .ok_or_else(|| CalmError::BadRequest(format!("unknown operation kind {kind}")))
    }

    async fn drive_one(&self, adapter: Arc<dyn ProviderAdapter>, op: Operation) -> Result<()> {
        match op.phase.clone() {
            Phase::Pending => {
                let prepared = self
                    .repo
                    .prepare_tx_and_advance(&op, adapter.as_ref())
                    .await;
                let Some((_next, events)) = (match prepared {
                    Ok(prepared) => prepared,
                    Err(e) => {
                        if let Some((last_error, last_error_class)) = client_failure_parts(&e) {
                            if let Some(result) = self
                                .repo
                                .mark_failed(
                                    &op,
                                    last_error,
                                    PhaseTag::Pending,
                                    Some(last_error_class.to_string()),
                                )
                                .await?
                            {
                                self.completion.complete(result);
                            } else {
                                log_lost_lease(&op, PhaseTag::Failed);
                            }
                            return Ok(());
                        }
                        return Err(e);
                    }
                }) else {
                    log_lost_lease(&op, PhaseTag::TxCommitted);
                    return Ok(());
                };
                for envelope in events {
                    self.events.emit_envelope(envelope);
                }
                Ok(())
            }
            Phase::TxCommitted => {
                if adapter.phases().contains(&PhaseTag::AppServerInteract) {
                    let output = required_output(&op)?;
                    let kind = adapter.app_server_interact_kind(output, &op)?;
                    if self
                        .repo
                        .set_phase(&op, Phase::AppServerInteract { kind })
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::AppServerInteract);
                    }
                    return Ok(());
                }
                if !adapter.phases().contains(&PhaseTag::SpawnStarted) {
                    let output = required_output(&op)?.clone();
                    match adapter
                        .spawn_side_effect(&output, &op, &self.spawn_ctx)
                        .await
                    {
                        Ok(SpawnOutcome::Ready(_handle)) => {
                            if let Some(result) = self.repo.set_phase(&op, Phase::Succeeded).await?
                            {
                                if let Some(result) = operation_result_from(&result)? {
                                    self.completion.complete(result);
                                }
                            } else {
                                log_lost_lease(&op, PhaseTag::Succeeded);
                            }
                        }
                        Ok(SpawnOutcome::Parked { .. }) => {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::TxCommitted,
                                "adapter returned parked from tx_committed spawn branch".into(),
                                output,
                            )
                            .await?;
                        }
                        Err(e) => {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::TxCommitted,
                                e.to_string(),
                                output,
                            )
                            .await?;
                        }
                    }
                    return Ok(());
                }
                if self
                    .repo
                    .set_phase(&op, Phase::SpawnStarted)
                    .await?
                    .is_none()
                {
                    log_lost_lease(&op, PhaseTag::SpawnStarted);
                }
                Ok(())
            }
            Phase::AppServerInteract { .. } => {
                let mut output = required_output(&op)?.clone();
                match adapter
                    .app_server_interact(&mut output, &op, &self.spawn_ctx)
                    .await
                {
                    Ok(AppServerInteractOutcome::NotApplicable) => {
                        if self
                            .repo
                            .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnStarted);
                        }
                    }
                    Ok(
                        AppServerInteractOutcome::MintedAndAwaited { .. }
                        | AppServerInteractOutcome::RegisteredPendingForLaterAttribution { .. },
                    ) => {
                        if self
                            .repo
                            .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnStarted);
                        }
                    }
                    Err(e) => {
                        self.fail_with_compensation(
                            adapter.as_ref(),
                            op,
                            PhaseTag::AppServerInteract,
                            e.to_string(),
                            output,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            Phase::SpawnStarted => {
                let output = required_output(&op)?.clone();
                match adapter
                    .spawn_side_effect(&output, &op, &self.spawn_ctx)
                    .await
                {
                    Ok(SpawnOutcome::Ready(_handle)) => {
                        if self
                            .repo
                            .set_phase(&op, Phase::SpawnSucceeded)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnSucceeded);
                        }
                    }
                    Ok(SpawnOutcome::Parked {
                        deadline_ms,
                        observer,
                    }) => {
                        if !adapter.phases().contains(&PhaseTag::Parked) {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::SpawnStarted,
                                "adapter returned parked without declaring parked phase".into(),
                                output,
                            )
                            .await?;
                            return Ok(());
                        }
                        if self.repo.set_parked(&op, deadline_ms).await?.is_some() {
                            tokio::spawn(observer);
                            return Ok(());
                        }

                        let current = self.repo.get_operation(&op.id).await?;
                        let still_holds_lease = current
                            .as_ref()
                            .map(|row| {
                                row.lease_owner == op.lease_owner
                                    && matches!(row.phase, Phase::SpawnStarted)
                            })
                            .unwrap_or(false);
                        let missing_artifacts = current
                            .as_ref()
                            .map(|row| row.spawn_artifacts.is_none())
                            .unwrap_or(false);
                        if still_holds_lease && missing_artifacts {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::SpawnStarted,
                                "adapter parked operation without recording spawn artifacts".into(),
                                output,
                            )
                            .await?;
                        } else {
                            log_lost_lease(&op, PhaseTag::Parked);
                        }
                    }
                    Err(e) => {
                        self.fail_with_compensation(
                            adapter.as_ref(),
                            op,
                            PhaseTag::SpawnStarted,
                            e.to_string(),
                            output,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            Phase::SpawnSucceeded => {
                if let Some(result) = self.repo.set_phase(&op, Phase::Succeeded).await? {
                    if let Some(result) = operation_result_from(&result)? {
                        self.completion.complete(result);
                    }
                } else {
                    log_lost_lease(&op, PhaseTag::Succeeded);
                }
                Ok(())
            }
            Phase::Compensating => {
                if let Some(result) = self
                    .resume_compensation(adapter.as_ref(), op.clone())
                    .await?
                {
                    self.completion.complete(result);
                } else {
                    log_lost_lease(&op, PhaseTag::Failed);
                }
                Ok(())
            }
            Phase::Parked => {
                tracing::warn!(
                    op_id = %op.id,
                    "parked operation reached drive_one; parked rows are excluded from drive claims"
                );
                Ok(())
            }
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. } => {
                if let Some(result) = operation_result_from(&op)? {
                    self.completion.complete(result);
                }
                Ok(())
            }
        }
    }

    async fn fail_with_compensation(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
        from_phase: PhaseTag,
        reason: String,
        output: TxOutput,
    ) -> Result<()> {
        let state = adapter
            .plan_compensation(from_phase, &reason, &output, &op)
            .await?;
        if self
            .repo
            .set_compensating(&op, &state, &output)
            .await?
            .is_none()
        {
            log_lost_lease(&op, PhaseTag::Compensating);
        }
        Ok(())
    }

    async fn resume_compensation(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
    ) -> Result<Option<OperationResult>> {
        let state = op
            .compensation_state
            .clone()
            .ok_or_else(|| {
                CalmError::Internal(format!("operation {} missing compensation_state", op.id))
            })
            .and_then(|value| {
                serde_json::from_value::<CompensationStateVersioned>(value).map_err(CalmError::from)
            })?;
        let output = required_output(&op)?.clone();
        let reason = state.reason.clone();
        let from_phase = state.from_phase;
        match self
            .apply_compensation_steps(adapter, op.clone(), state, output)
            .await
        {
            Ok(()) => {
                self.repo
                    .mark_failed(&op, reason, from_phase, Some("internal".into()))
                    .await
            }
            Err(e) => {
                self.repo
                    .mark_stuck(
                        &op,
                        format!("compensation failed: {e}"),
                        PhaseTag::Compensating,
                    )
                    .await
            }
        }
    }

    async fn apply_compensation_steps(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
        mut state: CompensationStateVersioned,
        output: TxOutput,
    ) -> Result<()> {
        for idx in 0..state.steps.len() {
            if state.steps[idx].completed {
                continue;
            }
            match adapter
                .compensate_step(&state.steps[idx], &output, &op, &self.spawn_ctx)
                .await
            {
                Ok(()) => {
                    state.steps[idx].completed = true;
                    state.steps[idx].last_error = None;
                    if self
                        .repo
                        .update_compensation_state(&op, &state)
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::Compensating);
                        return Ok(());
                    }
                }
                Err(e) => {
                    state.steps[idx].attempts += 1;
                    state.steps[idx].last_error = Some(e.to_string());
                    if self
                        .repo
                        .update_compensation_state(&op, &state)
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::Compensating);
                        return Ok(());
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    async fn apply_parked_sweep(&self, op: Operation) -> Result<()> {
        self.apply_parked_sweep_with_claim(op, ParkedClaimMode::SteadyState)
            .await
    }

    async fn sweep_parked_for_boot(&self) -> Result<()> {
        self.sweep_parked_with_claim(ParkedClaimMode::Boot).await
    }

    async fn sweep_parked_with_claim(&self, claim_mode: ParkedClaimMode) -> Result<()> {
        let rows = self.repo.parked_operations().await?;
        for op in rows {
            if let Err(e) = self
                .apply_parked_sweep_with_claim(op.clone(), claim_mode)
                .await
            {
                tracing::error!(
                    op_id = %op.id,
                    error = %e,
                    "parked operation sweep failed; continuing"
                );
            }
        }
        Ok(())
    }

    async fn claim_parked_with_mode(
        &self,
        op_id: &str,
        claim_mode: ParkedClaimMode,
    ) -> Result<Option<Operation>> {
        match claim_mode {
            ParkedClaimMode::SteadyState => self.repo.claim_parked(op_id).await,
            ParkedClaimMode::Boot => self.repo.claim_parked_for_boot(op_id).await,
        }
    }

    async fn apply_parked_sweep_with_claim(
        &self,
        op: Operation,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        if !matches!(op.phase, Phase::Parked) {
            return Ok(());
        }
        let Some(deadline_ms) = op.parked_deadline_ms else {
            if let Some(claimed) = self.claim_parked_with_mode(&op.id, claim_mode).await? {
                self.fail_claimed_parked(
                    claimed,
                    "parked operation missing deadline".into(),
                    Some("parked_deadline".into()),
                )
                .await?;
            }
            return Ok(());
        };
        if now_ms() > deadline_ms {
            return self
                .apply_parked_past_deadline_with_claim(&op.id, claim_mode)
                .await;
        }
        self.apply_parked_pre_deadline_probe(op, claim_mode).await
    }

    async fn apply_parked_pre_deadline_probe(
        &self,
        op: Operation,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        let Some(artifacts) = op.spawn_artifacts.clone() else {
            return Ok(());
        };
        if parked_artifacts_alive(&artifacts) {
            return Ok(());
        }
        let adapter = self.adapter(&op.kind)?;
        match adapter
            .recover_parked(
                &op,
                &artifacts,
                false,
                RecoveryMode::PreDeadlineProbe,
                &self.spawn_ctx,
            )
            .await?
        {
            ParkedRecovery::Complete(outcome) => {
                self.complete_parked_and_publish(&op.id, &outcome).await?;
            }
            // Dead work with NO recoverable outcome fails now (PR #685
            // round-2 F2): leaving it parked would sit until
            // `parked_deadline_ms` and then be misclassified as a
            // deadline failure (class `parked_deadline` — for the gate
            // adapter, `gate-timeout` instead of the true
            // `gate-infra`). Class `parked_dead` matches the boot-arm
            // semantics for the same state. Racing a live observer's
            // in-flight completion is interlocked exactly like the
            // past-deadline arm (#653 §4.4 orderings): a verdict that
            // commits first makes this claim miss; once the claim
            // lands, the lease-fenced `mark_failed` wins and the
            // observer's completion rolls back on `AlreadyResolved`.
            ParkedRecovery::Fail { reason } => {
                let Some(claimed) = self.claim_parked_with_mode(&op.id, claim_mode).await? else {
                    return Ok(());
                };
                let alive = parked_artifacts_alive(&artifacts);
                self.kill_recheck_then_fail_parked(
                    claimed,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    reason,
                    Some("parked_dead".into()),
                )
                .await?;
            }
            ParkedRecovery::LeaveParked => {}
        }
        Ok(())
    }

    async fn apply_parked_past_deadline_with_claim(
        &self,
        op_id: &str,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        let Some(op) = self.claim_parked_with_mode(op_id, claim_mode).await? else {
            return Ok(());
        };
        let Some(artifacts) = op.spawn_artifacts.clone() else {
            return self
                .fail_claimed_parked(
                    op,
                    "parked operation missing spawn artifacts".into(),
                    Some("parked_deadline".into()),
                )
                .await;
        };
        let adapter = self.adapter(&op.kind)?;
        let alive = parked_artifacts_alive(&artifacts);
        match adapter
            .recover_parked(
                &op,
                &artifacts,
                alive,
                RecoveryMode::PastDeadline,
                &self.spawn_ctx,
            )
            .await?
        {
            ParkedRecovery::Complete(outcome) => {
                kill_parked_group_if_alive(&artifacts, alive);
                self.complete_parked_and_publish(&op.id, &outcome).await?;
            }
            ParkedRecovery::Fail { reason } => {
                self.kill_recheck_then_fail_parked(
                    op,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    reason,
                    Some("parked_deadline".into()),
                )
                .await?;
            }
            ParkedRecovery::LeaveParked => {
                tracing::warn!(
                    op_id = %op.id,
                    "adapter returned LeaveParked during past-deadline enforcement"
                );
                self.kill_recheck_then_fail_parked(
                    op,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    "parked deadline exceeded".into(),
                    Some("parked_deadline".into()),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn complete_parked_and_publish(
        &self,
        op_id: &OperationId,
        outcome: &ParkedOutcome,
    ) -> Result<Option<OperationResult>> {
        let pool = self.repo.sqlite_pool();
        let mut tx = begin_immediate_tx(&pool).await?;
        match complete_parked_tx(&mut tx, op_id, outcome).await? {
            ParkedCompletion::Completed(result) => {
                tx.commit().await?;
                self.publish_completion(result.clone());
                Ok(Some(result))
            }
            ParkedCompletion::AlreadyResolved { .. } => {
                tx.rollback().await?;
                Ok(None)
            }
        }
    }

    async fn kill_recheck_then_fail_parked(
        &self,
        op: Operation,
        adapter: &dyn ProviderAdapter,
        artifacts: SpawnArtifacts,
        alive: bool,
        reason: String,
        last_error_class: Option<String>,
    ) -> Result<()> {
        if alive {
            kill_parked_group_if_alive(&artifacts, true);
            match adapter
                .recover_parked(
                    &op,
                    &artifacts,
                    false,
                    RecoveryMode::PastDeadline,
                    &self.spawn_ctx,
                )
                .await?
            {
                ParkedRecovery::Complete(outcome) => {
                    self.complete_parked_and_publish(&op.id, &outcome).await?;
                    return Ok(());
                }
                ParkedRecovery::Fail { .. } | ParkedRecovery::LeaveParked => {}
            }
        }
        if let Some(result) = self
            .repo
            .mark_failed(&op, reason, PhaseTag::Parked, last_error_class)
            .await?
        {
            self.publish_completion(result);
        } else {
            log_lost_lease(&op, PhaseTag::Failed);
        }
        Ok(())
    }

    async fn fail_claimed_parked(
        &self,
        op: Operation,
        reason: String,
        last_error_class: Option<String>,
    ) -> Result<()> {
        if let Some(result) = self
            .repo
            .mark_failed(&op, reason, PhaseTag::Parked, last_error_class)
            .await?
        {
            self.publish_completion(result);
        } else {
            log_lost_lease(&op, PhaseTag::Failed);
        }
        Ok(())
    }

    async fn plan_recovery_for(
        &self,
        _adapter: &dyn ProviderAdapter,
        op: &Operation,
    ) -> Result<RecoveryItem> {
        let item = match &op.phase {
            Phase::Pending
            | Phase::TxCommitted
            | Phase::AppServerInteract { .. }
            | Phase::SpawnStarted
            | Phase::SpawnSucceeded => RecoveryItem::Recover {
                op_id: op.id.clone(),
                from_phase: op.phase.clone(),
                action: format!("drive from {}", op.phase.tag().as_str()),
            },
            Phase::Parked => RecoveryItem::VerifyParked {
                op_id: op.id.clone(),
            },
            Phase::Compensating => RecoveryItem::Compensate {
                op_id: op.id.clone(),
                reason: op
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "resume compensation".into()),
            },
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. } => RecoveryItem::Skip {
                op_id: op.id.clone(),
                reason: "terminal state".into(),
            },
        };
        Ok(item)
    }

    async fn apply_recovery_item(&self, item: RecoveryItem) -> Result<()> {
        match item {
            RecoveryItem::Recover {
                op_id, from_phase, ..
            } => {
                let Some(op) = self.repo.claim_operation_for_recovery(&op_id).await? else {
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                if let Err(e) = self.drive_one(adapter, op.clone()).await {
                    if let Some(result) = self
                        .repo
                        .mark_stuck(
                            &op,
                            format!("operation recovery apply failed: {e}"),
                            from_phase.tag(),
                        )
                        .await?
                    {
                        self.completion.complete(result);
                    } else {
                        log_lost_lease(&op, PhaseTag::Stuck);
                    }
                }
                Ok(())
            }
            RecoveryItem::Compensate { op_id, .. } => {
                let Some(op) = self.repo.claim_operation_for_recovery(&op_id).await? else {
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                match self.resume_compensation(adapter.as_ref(), op.clone()).await {
                    Ok(Some(result)) => self.completion.complete(result),
                    Ok(None) => log_lost_lease(&op, PhaseTag::Failed),
                    Err(e) => {
                        if let Some(result) = self
                            .repo
                            .mark_stuck(
                                &op,
                                format!("operation compensation recovery failed: {e}"),
                                PhaseTag::Compensating,
                            )
                            .await?
                        {
                            self.completion.complete(result);
                        } else {
                            log_lost_lease(&op, PhaseTag::Stuck);
                        }
                    }
                }
                Ok(())
            }
            RecoveryItem::VerifyParked { op_id } => {
                let Some(op) = self.repo.get_operation(&op_id).await? else {
                    return Ok(());
                };
                if !matches!(op.phase, Phase::Parked) {
                    return Ok(());
                }
                if op
                    .parked_deadline_ms
                    .is_some_and(|deadline| now_ms() > deadline)
                {
                    self.apply_parked_past_deadline_with_claim(&op_id, ParkedClaimMode::Boot)
                        .await?;
                    return Ok(());
                }
                let Some(artifacts) = op.spawn_artifacts.clone() else {
                    if let Some(claimed) = self
                        .claim_parked_with_mode(&op_id, ParkedClaimMode::Boot)
                        .await?
                    {
                        self.fail_claimed_parked(
                            claimed,
                            "parked operation missing spawn artifacts".into(),
                            Some("parked_dead".into()),
                        )
                        .await?;
                    }
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                let alive = parked_artifacts_alive(&artifacts);
                match adapter
                    .recover_parked(&op, &artifacts, alive, RecoveryMode::Boot, &self.spawn_ctx)
                    .await?
                {
                    ParkedRecovery::LeaveParked => {
                        self.repo.clear_parked_lease_for_boot(&op_id).await?;
                    }
                    ParkedRecovery::Complete(outcome) => {
                        kill_parked_group_if_alive(&artifacts, alive);
                        self.complete_parked_and_publish(&op_id, &outcome).await?;
                    }
                    ParkedRecovery::Fail { reason } => {
                        let Some(claimed) = self
                            .claim_parked_with_mode(&op_id, ParkedClaimMode::Boot)
                            .await?
                        else {
                            return Ok(());
                        };
                        let alive = parked_artifacts_alive(&artifacts);
                        self.kill_recheck_then_fail_parked(
                            claimed,
                            adapter.as_ref(),
                            artifacts,
                            alive,
                            reason,
                            Some("parked_dead".into()),
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            RecoveryItem::Skip { .. } => Ok(()),
        }
    }
}

#[async_trait]
impl OperationRepo for SqlxOperationRepo {
    fn sqlite_pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    async fn assert_sqlite_version(&self) -> Result<()> {
        let row = sqlx::query("SELECT sqlite_version() AS version")
            .fetch_one(&self.pool)
            .await?;
        let version: String = row.try_get("version")?;
        if sqlite_version_at_least(&version, 3, 30) {
            return Ok(());
        }
        Err(CalmError::Internal(
            "SQLite < 3.30 does not support partial unique index; upgrade required".into(),
        ))
    }

    async fn insert_operation(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        if let Some(idempotency_key) = key.idempotency_key.as_deref()
            && let Some(existing) = self.find_by_kind_idempotency(kind, idempotency_key).await?
        {
            if existing.payload_hash == key.payload_hash {
                return Ok(existing.id);
            }
            return Err(idempotency_payload_conflict(Some(idempotency_key)));
        }

        let id = new_id();
        let now = now_ms();
        let (target_type, target_id, target_json) = target_from_payload(&payload);
        let target_json_text = serde_json::to_string(&target_json)?;
        let payload_json_text = serde_json::to_string(&payload)?;
        let inserted = sqlx::query(
            r#"INSERT INTO operations (
                   id, operation_key, kind, idempotency_key, payload_hash,
                   target_type, target_id, target_json, payload_json,
                   phase, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)"#,
        )
        .bind(&id)
        .bind(&key.operation_key)
        .bind(kind)
        .bind(&key.idempotency_key)
        .bind(&key.payload_hash)
        .bind(&target_type)
        .bind(&target_id)
        .bind(&target_json_text)
        .bind(&payload_json_text)
        .bind(now)
        .execute(&self.pool)
        .await;

        match inserted {
            Ok(_) => Ok(id),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                if let Some(idempotency_key) = key.idempotency_key.as_deref()
                    && let Some(existing) =
                        self.find_by_kind_idempotency(kind, idempotency_key).await?
                {
                    if existing.payload_hash == key.payload_hash {
                        return Ok(existing.id);
                    }
                    return Err(idempotency_payload_conflict(Some(idempotency_key)));
                }
                Err(CalmError::Conflict(format!(
                    "operation key {} already exists",
                    key.operation_key
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn find_by_idempotency_key(
        &self,
        kind: &str,
        key: &OperationKey,
    ) -> Result<Option<Operation>> {
        let Some(idempotency_key) = key.idempotency_key.as_deref() else {
            return Ok(None);
        };
        self.find_by_kind_idempotency(kind, idempotency_key).await
    }

    async fn get_operation(&self, op_id: &str) -> Result<Option<Operation>> {
        self.find_by_id(op_id).await
    }

    async fn operation_result(&self, op_id: &str) -> Result<Option<OperationResult>> {
        let Some(op) = self.find_by_id(op_id).await? else {
            return Ok(None);
        };
        operation_result_from(&op)
    }

    async fn claim_drive_batch(&self, limit: i64) -> Result<Vec<Operation>> {
        let now = now_ms();
        let lease_owner = new_id();
        let lease_until = now + OPERATION_LEASE_MS;
        let mut tx = self.pool.begin().await?;
        let ids = sqlx::query(
            r#"SELECT id
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'compensating'
               )
               AND (lease_until_ms IS NULL OR lease_until_ms < ?1)
               ORDER BY created_at_ms ASC
               LIMIT ?2"#,
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;

        let mut claimed = Vec::new();
        for row in ids {
            let id: String = row.try_get("id")?;
            let result = sqlx::query(
                r#"UPDATE operations
                   SET lease_owner = ?1,
                       lease_until_ms = ?2,
                       updated_at_ms = ?3
                   WHERE id = ?4
                     AND phase IN (
                       'pending',
                       'tx_committed',
                       'app_server_interact',
                       'spawn_started',
                       'spawn_succeeded',
                       'compensating'
                     )
                     AND (lease_until_ms IS NULL OR lease_until_ms < ?3)"#,
            )
            .bind(&lease_owner)
            .bind(lease_until)
            .bind(now)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() == 1 {
                let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
                    .bind(&id)
                    .fetch_one(&mut *tx)
                    .await?;
                claimed.push(operation_from_row(&row)?);
            }
        }
        tx.commit().await?;
        Ok(claimed)
    }

    async fn abandoned_running_operations_on_boot(&self) -> Result<Vec<Operation>> {
        let rows = sqlx::query(
            r#"SELECT *
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'parked',
                 'compensating'
               )
               ORDER BY created_at_ms ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    async fn abandoned_running_operations_steady_state(&self) -> Result<Vec<Operation>> {
        let now = now_ms();
        let rows = sqlx::query(
            r#"SELECT *
               FROM operations
               WHERE phase IN (
                 'pending',
                 'tx_committed',
                 'app_server_interact',
                 'spawn_started',
                 'spawn_succeeded',
                 'compensating'
               )
               AND (lease_until_ms IS NULL OR lease_until_ms < ?1)
               ORDER BY created_at_ms ASC"#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    async fn claim_operation_for_recovery(&self, op_id: &str) -> Result<Option<Operation>> {
        self.claim_operation_for_boot_recovery(op_id).await
    }

    async fn prepare_tx_and_advance(
        &self,
        op: &Operation,
        adapter: &dyn ProviderAdapter,
    ) -> Result<Option<(Operation, Vec<BroadcastEnvelope>)>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let output = match adapter.prepare_tx(&mut tx, &op.payload, op).await {
            Ok(output) => output,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let events = output.post_commit_events.clone();
        let mut output_for_db = output.clone();
        output_for_db.post_commit_events.clear();
        let output_text = serde_json::to_string(&output_for_db)?;
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE operations
               SET tx_output_json = ?1,
                   target_type = ?2,
                   target_id = ?3,
                   target_json = ?4,
                   phase = 'tx_committed',
                   phase_detail_json = NULL,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   updated_at_ms = ?5
               WHERE id = ?6
                 AND lease_owner = ?7"#,
        )
        .bind(&output_text)
        .bind(&output.target_type)
        .bind(&output.target_id)
        .bind(serde_json::to_string(&json!({
            "type": output.target_type,
            "id": output.target_id,
        }))?)
        .bind(now)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            let _ = tx.rollback().await;
            return Ok(None);
        }
        tx.commit().await?;
        let next = self
            .find_by_id(&op.id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))?;
        Ok(Some((next, events)))
    }

    async fn set_phase(&self, op: &Operation, phase: Phase) -> Result<Option<Operation>> {
        let (tag, detail) = phase.serialize_split();
        let detail_text = optional_json_text(detail.as_ref())?;
        let completed_at = matches!(
            phase,
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. }
        )
        .then(now_ms);
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = ?1,
                   phase_detail_json = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = COALESCE(?3, completed_at_ms),
                   updated_at_ms = ?4
               WHERE id = ?5
                 AND lease_owner = ?6"#,
        )
        .bind(tag.as_str())
        .bind(detail_text)
        .bind(completed_at)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn set_phase_and_tx_output(
        &self,
        op: &Operation,
        phase: Phase,
        output: &TxOutput,
    ) -> Result<Option<Operation>> {
        let (tag, detail) = phase.serialize_split();
        let detail_text = optional_json_text(detail.as_ref())?;
        let completed_at = matches!(
            phase,
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. }
        )
        .then(now_ms);
        let mut output_for_db = output.clone();
        output_for_db.post_commit_events.clear();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = ?1,
                   phase_detail_json = ?2,
                   tx_output_json = ?3,
                   target_type = ?4,
                   target_id = ?5,
                   target_json = ?6,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = COALESCE(?7, completed_at_ms),
                   updated_at_ms = ?8
               WHERE id = ?9
                 AND lease_owner = ?10"#,
        )
        .bind(tag.as_str())
        .bind(detail_text)
        .bind(serde_json::to_string(&output_for_db)?)
        .bind(&output.target_type)
        .bind(&output.target_id)
        .bind(serde_json::to_string(&json!({
            "type": output.target_type,
            "id": output.target_id,
        }))?)
        .bind(completed_at)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn set_compensating(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
        output: &TxOutput,
    ) -> Result<Option<Operation>> {
        let text = serde_json::to_string(state)?;
        let mut output_for_db = output.clone();
        output_for_db.post_commit_events.clear();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'compensating',
                   phase_detail_json = ?1,
                   compensation_state = ?2,
                   tx_output_json = ?3,
                   target_type = ?4,
                   target_id = ?5,
                   target_json = ?6,
                   last_error = ?7,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   updated_at_ms = ?8
               WHERE id = ?9
                 AND lease_owner = ?10"#,
        )
        .bind(serde_json::to_string(&json!({
            "from_phase": state.from_phase,
            "reason": state.reason,
        }))?)
        .bind(text)
        .bind(serde_json::to_string(&output_for_db)?)
        .bind(&output.target_type)
        .bind(&output.target_id)
        .bind(serde_json::to_string(&json!({
            "type": output.target_type,
            "id": output.target_id,
        }))?)
        .bind(&state.reason)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn update_compensation_state(
        &self,
        op: &Operation,
        state: &CompensationStateVersioned,
    ) -> Result<Option<Operation>> {
        let result = sqlx::query(
            r#"UPDATE operations
               SET compensation_state = ?1,
                   updated_at_ms = ?2
               WHERE id = ?3
                 AND lease_owner = ?4"#,
        )
        .bind(serde_json::to_string(state)?)
        .bind(now_ms())
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(&op.id)
            .await?
            .map(Some)
            .ok_or_else(|| CalmError::Internal(format!("operation {} vanished", op.id)))
    }

    async fn mark_failed(
        &self,
        op: &Operation,
        last_error: String,
        from_phase: PhaseTag,
        last_error_class: Option<String>,
    ) -> Result<Option<OperationResult>> {
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'failed',
                   phase_detail_json = ?1,
                   last_error = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = ?3,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND lease_owner = ?5"#,
        )
        .bind(serde_json::to_string(&json!({
            "from_phase": from_phase,
            "last_error_class": last_error_class,
        }))?)
        .bind(&last_error)
        .bind(now)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        Ok(Some(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Failed {
                last_error,
                from_phase,
                last_error_class,
            },
        }))
    }

    async fn mark_stuck(
        &self,
        op: &Operation,
        reason: String,
        from_phase: PhaseTag,
    ) -> Result<Option<OperationResult>> {
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE operations
               SET phase = 'stuck',
                   phase_detail_json = ?1,
                   last_error = ?2,
                   lease_owner = NULL,
                   lease_until_ms = NULL,
                   completed_at_ms = ?3,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND lease_owner = ?5"#,
        )
        .bind(serde_json::to_string(&json!({
            "reason": reason,
            "since": now,
            "from_phase": from_phase,
        }))?)
        .bind(&reason)
        .bind(now)
        .bind(&op.id)
        .bind(required_lease_owner(op)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        Ok(Some(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Stuck { reason, from_phase },
        }))
    }
}

impl SqlxOperationRepo {
    async fn find_by_id(&self, op_id: &str) -> Result<Option<Operation>> {
        let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(operation_from_row).transpose()
    }

    async fn find_by_kind_idempotency(
        &self,
        kind: &str,
        idempotency_key: &str,
    ) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT * FROM operations WHERE kind = ?1 AND idempotency_key = ?2 LIMIT 1",
        )
        .bind(kind)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(operation_from_row).transpose()
    }
}

pub(crate) async fn checkpoint_app_server_interact_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    kind: AppServerInteractKind,
    output: &TxOutput,
) -> Result<()> {
    let phase = Phase::AppServerInteract { kind };
    let (_tag, detail) = phase.serialize_split();
    let detail_text = optional_json_text(detail.as_ref())?;
    let mut output_for_db = output.clone();
    output_for_db.post_commit_events.clear();
    let result = sqlx::query(
        r#"UPDATE operations
           SET phase_detail_json = ?1,
               tx_output_json = ?2,
               target_type = ?3,
               target_id = ?4,
               target_json = ?5,
               updated_at_ms = ?6
           WHERE id = ?7
             AND phase = 'app_server_interact'
             AND lease_owner = ?8"#,
    )
    .bind(detail_text)
    .bind(serde_json::to_string(&output_for_db)?)
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(serde_json::to_string(&json!({
        "type": output.target_type,
        "id": output.target_id,
    }))?)
    .bind(now_ms())
    .bind(&op.id)
    .bind(required_lease_owner(op)?)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(CalmError::Internal(format!(
            "operation {} lost lease while checkpointing app_server_interact",
            op.id
        )));
    }
    Ok(())
}

// Future parking consumers resolve operation ids inside their own completion tx.
#[allow(dead_code)]
pub(crate) async fn find_operation_id_by_kind_idempotency_tx(
    tx: &mut Tx<'_>,
    kind: &str,
    idempotency_key: &str,
) -> Result<Option<OperationId>> {
    let id = sqlx::query_scalar(
        "SELECT id FROM operations WHERE kind = ?1 AND idempotency_key = ?2 LIMIT 1",
    )
    .bind(kind)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(id)
}

pub(crate) async fn complete_parked_tx(
    tx: &mut Tx<'_>,
    op_id: &OperationId,
    outcome: &ParkedOutcome,
) -> Result<ParkedCompletion> {
    let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(CalmError::NotFound(format!("operation {op_id} not found")));
    };
    let op = operation_from_row(&row)?;
    if !matches!(op.phase, Phase::Parked) {
        return Ok(ParkedCompletion::AlreadyResolved {
            phase: op.phase.tag(),
        });
    }

    let mut output = required_output(&op)?.clone();
    output.post_commit_events.clear();
    let (phase, phase_detail, last_error) = match outcome {
        ParkedOutcome::Succeeded { result } => {
            output.result = result.clone();
            ("succeeded", None, None)
        }
        ParkedOutcome::Failed {
            last_error,
            last_error_class,
        } => (
            "failed",
            Some(serde_json::to_string(&json!({
                "from_phase": PhaseTag::Parked,
                "last_error_class": last_error_class,
            }))?),
            Some(last_error.clone()),
        ),
    };
    let output_text = serde_json::to_string(&output)?;
    let now = now_ms();
    let result = sqlx::query(
        r#"UPDATE operations
           SET phase = ?1,
               phase_detail_json = ?2,
               tx_output_json = ?3,
               last_error = ?4,
               lease_owner = NULL,
               lease_until_ms = NULL,
               parked_deadline_ms = NULL,
               completed_at_ms = ?5,
               updated_at_ms = ?5
           WHERE id = ?6
             AND phase = 'parked'"#,
    )
    .bind(phase)
    .bind(phase_detail)
    .bind(output_text)
    .bind(last_error.clone())
    .bind(now)
    .bind(op_id)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        let phase = sqlx::query_scalar::<_, String>("SELECT phase FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_one(&mut **tx)
            .await?;
        return Ok(ParkedCompletion::AlreadyResolved {
            phase: PhaseTag::from_db_str(&phase)?,
        });
    }

    let completed = match outcome {
        ParkedOutcome::Succeeded { result } => OperationResult {
            op_id: op_id.clone(),
            outcome: OperationOutcome::Succeeded {
                result: result.clone(),
            },
        },
        ParkedOutcome::Failed {
            last_error,
            last_error_class,
        } => OperationResult {
            op_id: op_id.clone(),
            outcome: OperationOutcome::Failed {
                last_error: last_error.clone(),
                from_phase: PhaseTag::Parked,
                last_error_class: last_error_class.clone(),
            },
        },
    };
    Ok(ParkedCompletion::Completed(completed))
}

#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub async fn complete_parked_for_test(
    pool: &SqlitePool,
    op_id: &OperationId,
    outcome: &ParkedOutcome,
) -> Result<ParkedCompletion> {
    let mut tx = begin_immediate_tx(pool).await?;
    let completion = complete_parked_tx(&mut tx, op_id, outcome).await?;
    tx.commit().await?;
    Ok(completion)
}

async fn fetch_claimed_parked(
    pool: &SqlitePool,
    op_id: &str,
    lease_owner: &str,
) -> Result<Option<Operation>> {
    let row = sqlx::query(
        r#"SELECT *
           FROM operations
           WHERE id = ?1
             AND lease_owner = ?2
             AND phase = 'parked'"#,
    )
    .bind(op_id)
    .bind(lease_owner)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(operation_from_row).transpose()
}

fn operation_from_row(row: &SqliteRow) -> Result<Operation> {
    let target_json: String = row.try_get("target_json")?;
    let payload_json: String = row.try_get("payload_json")?;
    let phase_text: String = row.try_get("phase")?;
    let phase_detail_json: Option<String> = row.try_get("phase_detail_json")?;
    let phase_detail = phase_detail_json
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    let tx_output_json: Option<String> = row.try_get("tx_output_json")?;
    let tx_output = tx_output_json
        .as_deref()
        .map(serde_json::from_str::<TxOutput>)
        .transpose()?;
    let compensation_state_text: Option<String> = row.try_get("compensation_state")?;
    let compensation_state = compensation_state_text
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    let spawn_artifacts_text: Option<String> = row.try_get("spawn_artifacts_json")?;
    let spawn_artifacts =
        spawn_artifacts_text.as_deref().and_then(|text| {
            match serde_json::from_str::<SpawnArtifacts>(text) {
                Ok(artifacts) => Some(artifacts),
                Err(e) => {
                    tracing::warn!(
                        operation_id = ?row.try_get::<String, _>("id").ok(),
                        error = %e,
                        "operation row has invalid spawn_artifacts_json"
                    );
                    None
                }
            }
        });
    Ok(Operation {
        id: row.try_get("id")?,
        operation_key: row.try_get("operation_key")?,
        kind: row.try_get("kind")?,
        idempotency_key: row.try_get("idempotency_key")?,
        payload_hash: row.try_get("payload_hash")?,
        target_type: row.try_get("target_type")?,
        target_id: row.try_get("target_id")?,
        target: serde_json::from_str(&target_json)?,
        payload: serde_json::from_str(&payload_json)?,
        tx_output,
        phase: Phase::deserialize_join(&phase_text, phase_detail.as_ref())?,
        phase_detail,
        attempt: row.try_get("attempt")?,
        last_error: row.try_get("last_error")?,
        compensation_state,
        lease_owner: row.try_get("lease_owner")?,
        lease_until_ms: row.try_get("lease_until_ms")?,
        spawn_artifacts,
        parked_at_ms: row.try_get("parked_at_ms")?,
        parked_deadline_ms: row.try_get("parked_deadline_ms")?,
    })
}

fn required_lease_owner(op: &Operation) -> Result<&str> {
    op.lease_owner.as_deref().ok_or_else(|| {
        CalmError::Internal(format!(
            "operation {} is not claimed by the current driver",
            op.id
        ))
    })
}

fn log_lost_lease(op: &Operation, intended_phase: PhaseTag) {
    tracing::warn!(
        op_id = %op.id,
        intended_phase = intended_phase.as_str(),
        "operation transition skipped because driver lost lease"
    );
}

fn parked_artifacts_alive(artifacts: &SpawnArtifacts) -> bool {
    verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id)
}

fn kill_parked_group_if_alive(artifacts: &SpawnArtifacts, alive: bool) {
    if !alive {
        return;
    }
    if parked_artifacts_alive(artifacts) {
        signal_process_group(artifacts.pgid, libc::SIGKILL);
    }
}

fn operation_result_from(op: &Operation) -> Result<Option<OperationResult>> {
    match &op.phase {
        Phase::Succeeded => {
            if let Some(detail) = &op.phase_detail
                && detail.get("completion").and_then(Value::as_str) == Some("idempotency_collision")
            {
                return Ok(Some(OperationResult {
                    op_id: op.id.clone(),
                    outcome: OperationOutcome::SucceededViaCollision {
                        existing_op_id: detail
                            .get("existing_operation_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        result: op
                            .tx_output
                            .as_ref()
                            .map(|o| o.result.clone())
                            .unwrap_or(Value::Null),
                    },
                }));
            }
            Ok(Some(OperationResult {
                op_id: op.id.clone(),
                outcome: OperationOutcome::Succeeded {
                    result: op
                        .tx_output
                        .as_ref()
                        .map(|o| o.result.clone())
                        .unwrap_or(Value::Null),
                },
            }))
        }
        Phase::Failed => Ok(Some(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Failed {
                last_error: op
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "operation failed".into()),
                from_phase: phase_detail_from_phase(op.phase_detail.as_ref()),
                last_error_class: error_class_from_phase(op.phase_detail.as_ref()),
            },
        })),
        Phase::Stuck { reason, .. } => Ok(Some(OperationResult {
            op_id: op.id.clone(),
            outcome: OperationOutcome::Stuck {
                reason: reason.clone(),
                from_phase: phase_detail_from_phase(op.phase_detail.as_ref()),
            },
        })),
        _ => Ok(None),
    }
}

fn phase_detail_from_phase(detail: Option<&Value>) -> PhaseTag {
    detail
        .and_then(|v| v.get("from_phase"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or(PhaseTag::Failed)
}

fn error_class_from_phase(detail: Option<&Value>) -> Option<String> {
    detail
        .and_then(|v| v.get("last_error_class"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn client_failure_parts(error: &CalmError) -> Option<(String, &'static str)> {
    match error {
        CalmError::BadRequest(message) => Some((message.clone(), "bad_request")),
        CalmError::NotFound(message) => Some((message.clone(), "not_found")),
        CalmError::Forbidden(message) => Some((message.clone(), "forbidden")),
        CalmError::Conflict(message) => Some((message.clone(), "conflict")),
        CalmError::Unauthorized => Some(("unauthorized".into(), "unauthorized")),
        // PR2: extend when codex/claude adapters land and can raise
        // plugin/reset-specific client errors from prepare-time validation.
        _ => None,
    }
}

fn required_output(op: &Operation) -> Result<&TxOutput> {
    op.tx_output
        .as_ref()
        .ok_or_else(|| CalmError::Internal(format!("operation {} missing tx_output_json", op.id)))
}

fn optional_json_text(value: Option<&Value>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

fn target_from_payload(payload: &Value) -> (String, Option<String>, Value) {
    if let Some(runtime_id) = payload.get("runtime_id").and_then(Value::as_str) {
        return (
            "runtime".to_string(),
            Some(runtime_id.to_string()),
            json!({ "type": "runtime", "id": runtime_id }),
        );
    }
    let wave_id = payload.get("wave_id").and_then(Value::as_str).or_else(|| {
        payload
            .get("request")
            .and_then(|request| request.get("wave_id"))
            .and_then(Value::as_str)
    });
    if let Some(wave_id) = wave_id {
        return (
            "wave".to_string(),
            Some(wave_id.to_string()),
            json!({ "type": "wave", "id": wave_id }),
        );
    }
    (
        "unknown".to_string(),
        None,
        json!({ "type": "unknown", "id": Value::Null }),
    )
}

fn sqlite_version_at_least(version: &str, want_major: u64, want_minor: u64) -> bool {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    (major, minor) >= (want_major, want_minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LegacyCompensationHarness {
        repo: Arc<crate::db::sqlite::SqlxRepo>,
        route_repo: Arc<dyn crate::db::RouteRepo>,
        spawn_ctx: SpawnCtx,
        output: TxOutput,
        op: Operation,
        card_id: String,
        runtime_id: String,
        events: EventBus,
    }

    async fn legacy_compensation_harness(
        card_kind: &str,
        session_kind: crate::session_projection_repo::WorkerSessionKind,
        agent_provider: Option<crate::session_projection_repo::AgentProvider>,
    ) -> LegacyCompensationHarness {
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let cove = crate::db::RepoSyncDomainRaw::cove_create(
            repo.as_ref(),
            crate::model::NewCove {
                name: "legacy compensation".into(),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = crate::db::RepoSyncDomainRaw::wave_create(
            repo.as_ref(),
            crate::model::NewWave {
                cove_id: cove.id,
                title: "legacy compensation".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            },
        )
        .await
        .unwrap();
        let card = crate::db::RepoSyncDomainRaw::card_create(
            repo.as_ref(),
            crate::model::NewCard {
                wave_id: wave.id,
                kind: card_kind.into(),
                sort: None,
                payload: json!({ "schemaVersion": 1 }),
            },
        )
        .await
        .unwrap();
        let runtime_id = new_id();
        let mut tx = repo.pool().begin().await.unwrap();
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            crate::session_projection_repo::WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: card.id.to_string(),
                kind: session_kind,
                agent_provider,
                status: crate::session_projection_repo::WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: None,
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

        let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
        let events = EventBus::new();
        let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let spawn_ctx = SpawnCtx::new(
            route_repo.clone(),
            operation_repo,
            Arc::new(DaemonClient::new_stub()),
            terminal_renderer,
            events.clone(),
            OperationCompletionBus::new(),
        );
        let card_id = card.id.to_string();

        LegacyCompensationHarness {
            repo,
            route_repo,
            spawn_ctx,
            output: TxOutput::new("card", Some(card_id.clone()), json!({})),
            op: Operation {
                id: new_id(),
                operation_key: new_id(),
                kind: format!("{card_kind}-test"),
                idempotency_key: Some(new_id()),
                payload_hash: new_id(),
                target_type: "card".into(),
                target_id: Some(card_id.clone()),
                target: json!({ "type": "card", "id": card_id }),
                payload: json!({}),
                tx_output: None,
                phase: Phase::Compensating,
                phase_detail: None,
                attempt: 0,
                last_error: None,
                compensation_state: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_artifacts: None,
                parked_at_ms: None,
                parked_deadline_ms: None,
            },
            card_id,
            runtime_id,
            events,
        }
    }

    async fn assert_legacy_failed_status_compensation(
        adapter: &dyn ProviderAdapter,
        harness: LegacyCompensationHarness,
    ) {
        let step = CompensationStep {
            op: "runtime_set_status_failed_for_card".into(),
            args: json!({ "card_id": harness.card_id }),
            completed: false,
            attempts: 0,
            last_error: None,
        };

        adapter
            .compensate_step(&step, &harness.output, &harness.op, &harness.spawn_ctx)
            .await
            .unwrap();

        let runtime =
            crate::session_projection_repo::WorkerSessionProjectionRepo::session_projection_by_id(
                harness.repo.as_ref(),
                &harness.runtime_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime.status,
            crate::session_projection_repo::WorkerSessionState::Failed
        );
    }

    #[tokio::test]
    async fn prompted_adapters_accept_legacy_failed_status_compensation_op() {
        let harness = legacy_compensation_harness(
            "codex",
            crate::session_projection_repo::WorkerSessionKind::CodexCard,
            Some(crate::session_projection_repo::AgentProvider::Codex),
        )
        .await;
        let repo: Arc<dyn crate::db::Repo> = harness.repo.clone();
        let adapter = crate::operation::codex_adapter::CodexAdapter::new(
            harness.route_repo.clone(),
            Arc::new(crate::state::CodexClient::new_stub()),
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
            Arc::new(
                crate::pending_codex_threads::PendingThreadStartRegistry::new(
                    repo,
                    harness.events.clone(),
                ),
            ),
            Arc::new(Mutex::new(())),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        assert_legacy_failed_status_compensation(&adapter, harness).await;

        let harness = legacy_compensation_harness(
            "claude",
            crate::session_projection_repo::WorkerSessionKind::ClaudeCard,
            Some(crate::session_projection_repo::AgentProvider::Claude),
        )
        .await;
        let adapter = crate::operation::claude_adapter::ClaudeAdapter::new(
            harness.route_repo.clone(),
            Arc::new(crate::state::CodexClient::new_stub()),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        assert_legacy_failed_status_compensation(&adapter, harness).await;

        let harness = legacy_compensation_harness(
            "claude",
            crate::session_projection_repo::WorkerSessionKind::ClaudeCard,
            Some(crate::session_projection_repo::AgentProvider::Claude),
        )
        .await;
        let adapter = crate::operation::claude_restart_adapter::ClaudeRestartAdapter::new(
            harness.route_repo.clone(),
            Arc::new(crate::state::CodexClient::new_stub()),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        assert_legacy_failed_status_compensation(&adapter, harness).await;
    }

    #[test]
    fn phase_split_round_trips_all_variants() {
        let cases = vec![
            Phase::Pending,
            Phase::TxCommitted,
            Phase::AppServerInteract {
                kind: AppServerInteractKind::MintAndAwait {
                    thread_id: Some("thread-1".into()),
                },
            },
            Phase::AppServerInteract {
                kind: AppServerInteractKind::RegisterPending {
                    entry_id: Some("pending-1".into()),
                },
            },
            Phase::SpawnStarted,
            Phase::SpawnSucceeded,
            Phase::Parked,
            Phase::Succeeded,
            Phase::Compensating,
            Phase::Failed,
            Phase::Stuck {
                reason: "needs operator".into(),
                since: 1_718_000_000,
            },
        ];

        for phase in cases {
            let (tag, detail) = phase.serialize_split();
            let joined = Phase::deserialize_join(tag.as_str(), detail.as_ref()).unwrap();
            assert_eq!(joined, phase);
        }
    }

    #[tokio::test]
    async fn migration_check_rejects_parked_without_artifacts_deadline() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let now = now_ms();
        let err = sqlx::query(
            r#"INSERT INTO operations (
                   id, operation_key, kind, payload_hash, target_type,
                   target_json, payload_json, phase, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'test-kind', 'hash', 'unknown',
                       '{"type":"unknown","id":null}', '{}', 'parked', ?3, ?3)"#,
        )
        .bind(new_id())
        .bind(new_id())
        .bind(now)
        .execute(sqlx_repo.pool())
        .await
        .unwrap_err();
        assert!(
            matches!(err, sqlx::Error::Database(_)),
            "parked row without artifacts/deadline must fail CHECK: {err}"
        );
    }

    #[tokio::test]
    async fn set_parked_requires_lease_and_artifacts_record_rejects_stale_lease() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let op = claimed_spawn_started_operation(&repo).await;
        let deadline = now_ms() + 10_000;

        assert!(
            repo.set_parked(&op, deadline).await.unwrap().is_none(),
            "parking without recorded artifacts must miss"
        );

        let mut stale = op.clone();
        stale.lease_owner = Some("stale-driver".into());
        assert!(
            repo.record_spawn_artifacts(&stale, &sample_spawn_artifacts())
                .await
                .is_err(),
            "stale lease cannot record artifacts"
        );

        repo.record_spawn_artifacts(&op, &sample_spawn_artifacts())
            .await
            .unwrap();
        let parked = repo
            .set_parked(&op, deadline)
            .await
            .unwrap()
            .expect("leased op with artifacts parks");
        assert_eq!(parked.phase, Phase::Parked);
        assert!(parked.lease_owner.is_none());
        assert!(parked.lease_until_ms.is_none());
        assert!(parked.spawn_artifacts.is_some());
        assert_eq!(parked.parked_deadline_ms, Some(deadline));
        assert!(parked.parked_at_ms.is_some());

        assert!(
            repo.set_parked(&op, deadline).await.unwrap().is_none(),
            "old lease cannot park again after lease clear"
        );
    }

    #[tokio::test]
    async fn complete_parked_tx_splices_result_and_double_complete_is_resolved() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let op = parked_operation(&repo, now_ms() + 10_000).await;

        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        let completion = complete_parked_tx(
            &mut tx,
            &op.id,
            &ParkedOutcome::Succeeded {
                result: json!({ "parked": "done" }),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(matches!(completion, ParkedCompletion::Completed(_)));

        let result = repo.operation_result(&op.id).await.unwrap().unwrap();
        assert!(matches!(
            result.outcome,
            OperationOutcome::Succeeded { ref result }
                if result == &json!({ "parked": "done" })
        ));

        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        let second = complete_parked_tx(
            &mut tx,
            &op.id,
            &ParkedOutcome::Succeeded {
                result: json!({ "ignored": true }),
            },
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert!(matches!(
            second,
            ParkedCompletion::AlreadyResolved {
                phase: PhaseTag::Succeeded
            }
        ));
    }

    #[tokio::test]
    async fn complete_after_compensating_and_cancel_after_complete_are_noops() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let op = parked_operation(&repo, now_ms() + 10_000).await;
        let claimed = repo.claim_parked(&op.id).await.unwrap().unwrap();
        let output = required_output(&claimed).unwrap().clone();
        let state = CompensationStateVersioned {
            version: 1,
            from_phase: PhaseTag::Parked,
            reason: "cancel".into(),
            steps: Vec::new(),
        };
        repo.set_compensating(&claimed, &state, &output)
            .await
            .unwrap()
            .expect("claim flips to compensating");

        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        let completion = complete_parked_tx(
            &mut tx,
            &op.id,
            &ParkedOutcome::Failed {
                last_error: "late".into(),
                last_error_class: Some("late".into()),
            },
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert!(matches!(
            completion,
            ParkedCompletion::AlreadyResolved {
                phase: PhaseTag::Compensating
            }
        ));

        let op = parked_operation(&repo, now_ms() + 10_000).await;
        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        assert!(matches!(
            complete_parked_tx(
                &mut tx,
                &op.id,
                &ParkedOutcome::Succeeded {
                    result: json!({ "ok": true }),
                },
            )
            .await
            .unwrap(),
            ParkedCompletion::Completed(_)
        ));
        tx.commit().await.unwrap();

        let completion = OperationCompletionBus::new();
        let route_repo: Arc<dyn crate::db::RouteRepo> = Arc::new(sqlx_repo);
        let operation_repo = Arc::new(repo);
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let runtime = OperationRuntime::new_unchecked(
            operation_repo.clone(),
            Vec::new(),
            EventBus::new(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                EventBus::new(),
                completion,
            ),
        );
        assert!(!runtime.cancel_parked(&op.id, "too late").await.unwrap());
    }

    #[tokio::test]
    async fn same_idempotency_key_different_hash_conflicts() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let key = OperationKey {
            operation_key: "op-a".into(),
            idempotency_key: Some("same-key".into()),
            payload_hash: "hash-a".into(),
        };
        let payload = json!({ "wave_id": "wave-a" });
        let first = repo
            .insert_operation("terminal-create", key, payload.clone())
            .await
            .unwrap();
        assert!(!first.is_empty());

        let err = repo
            .insert_operation(
                "terminal-create",
                OperationKey {
                    operation_key: "op-b".into(),
                    idempotency_key: Some("same-key".into()),
                    payload_hash: "hash-b".into(),
                },
                payload,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CalmError::Conflict(_)));
    }

    #[tokio::test]
    async fn operation_event_append_creates_wave_vcs_commit() {
        use crate::card_role_cache::CardRoleCache;
        use crate::db::prelude::*;
        use crate::db::sqlite::{
            append_decision_event_in_tx, begin_immediate_tx, card_create_with_id_tx,
        };
        use crate::event::{Event, EventScope};
        use crate::ids::{ActorId, CardId};
        use crate::model::{CardRole, NewCard, NewCove, NewWave};
        use crate::routes::theme::RequestTheme;
        use crate::wave_report::WaveReportPayload;
        use calm_truth::decision_gate::PermissiveGate;

        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let cove = sqlx_repo
            .cove_create(NewCove {
                name: "cove".into(),
                color: "#abcdef".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = sqlx_repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "wave".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let roles = CardRoleCache::new();
        let card_id = new_id();
        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        let report = card_create_with_id_tx(
            &mut tx,
            card_id.clone(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: None,
                payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
            },
            CardRole::ReportCard,
            false,
            &roles,
        )
        .await
        .unwrap();
        let scope = EventScope::Card {
            card: CardId::from(card_id),
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        };
        let event = Event::CardAdded(report);
        let event_id = append_decision_event_in_tx(
            &mut tx,
            &PermissiveGate,
            &ActorId::Kernel,
            &scope,
            None,
            &event,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let head = crate::wave_vcs::head(sqlx_repo.pool(), &wave.id)
            .await
            .unwrap()
            .expect("vcs head");
        let stored_event_id: i64 =
            sqlx::query_scalar("SELECT updated_event_id FROM wave_vcs_refs WHERE wave_id = ?1")
                .bind(wave.id.as_str())
                .fetch_one(sqlx_repo.pool())
                .await
                .unwrap();
        assert_eq!(stored_event_id, event_id);
        let author: Option<String> =
            sqlx::query_scalar("SELECT author FROM wave_vcs_commits WHERE hash = ?1")
                .bind(&head)
                .fetch_one(sqlx_repo.pool())
                .await
                .unwrap();
        assert_eq!(author.as_deref(), Some("kernel"));
        assert!(
            crate::wave_vcs::tree_at(sqlx_repo.pool(), &head)
                .await
                .unwrap()
                .expect("tree")
                .entries
                .contains_key("report.md")
        );
    }

    #[tokio::test]
    async fn set_phase_clears_lease_and_rejects_stale_owner() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let op_id = repo
            .insert_operation(
                "terminal-create",
                OperationKey {
                    operation_key: "phase-fence-op".into(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({ "wave_id": "wave-a" }),
            )
            .await
            .unwrap();
        let mut claimed = repo.claim_drive_batch(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        let op = claimed.pop().unwrap();
        assert!(op.lease_owner.is_some());

        let next = repo
            .set_phase(&op, Phase::TxCommitted)
            .await
            .unwrap()
            .expect("claimed owner advances");
        assert_eq!(next.phase, Phase::TxCommitted);
        assert!(next.lease_owner.is_none());
        assert!(next.lease_until_ms.is_none());

        let stale = repo.set_phase(&op, Phase::SpawnStarted).await.unwrap();
        assert!(
            stale.is_none(),
            "stale owner must not advance after set_phase clears the lease"
        );
        let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
        assert_eq!(stored.phase, Phase::TxCommitted);
    }

    #[tokio::test]
    async fn stale_driver_cannot_win_final_transition_after_reclaim() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let op_id = repo
            .insert_operation(
                "terminal-create",
                OperationKey {
                    operation_key: "final-fence-op".into(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({ "wave_id": "wave-a" }),
            )
            .await
            .unwrap();
        let now = now_ms();
        sqlx::query(
            r#"UPDATE operations
               SET phase = 'spawn_succeeded',
                   lease_owner = 'driver-a',
                   lease_until_ms = ?1,
                   updated_at_ms = ?2
               WHERE id = ?3"#,
        )
        .bind(now - 1)
        .bind(now)
        .bind(&op_id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();
        let stale_driver = repo.get_operation(&op_id).await.unwrap().unwrap();
        assert_eq!(stale_driver.lease_owner.as_deref(), Some("driver-a"));

        let mut claimed = repo.claim_drive_batch(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        let driver_b = claimed.pop().unwrap();
        assert_ne!(driver_b.lease_owner, stale_driver.lease_owner);

        let stale = repo
            .set_phase(&stale_driver, Phase::Succeeded)
            .await
            .unwrap();
        assert!(stale.is_none(), "driver A's stale final transition loses");
        let winner = repo
            .set_phase(&driver_b, Phase::Succeeded)
            .await
            .unwrap()
            .expect("driver B owns the final transition");
        assert_eq!(winner.phase, Phase::Succeeded);

        let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
        assert_eq!(stored.phase, Phase::Succeeded);
        assert!(stored.lease_owner.is_none());
    }

    #[tokio::test]
    async fn claim_drive_batch_excludes_parked_and_claim_parked_is_exact_phase() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let parked = parked_operation(&repo, now_ms() + 10_000).await;

        assert!(
            repo.claim_drive_batch(32).await.unwrap().is_empty(),
            "parked operations are not drive-claimable"
        );
        assert!(
            repo.claim_parked(&parked.id).await.unwrap().is_some(),
            "parked operations are claimable through the exact-phase path"
        );

        let compensating = parked_operation(&repo, now_ms() + 10_000).await;
        sqlx::query(
            "UPDATE operations SET phase = 'compensating', lease_owner = NULL WHERE id = ?1",
        )
        .bind(&compensating.id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();
        assert!(repo.claim_parked(&compensating.id).await.unwrap().is_none());

        let terminal = parked_operation(&repo, now_ms() + 10_000).await;
        sqlx::query("UPDATE operations SET phase = 'succeeded', lease_owner = NULL WHERE id = ?1")
            .bind(&terminal.id)
            .execute(sqlx_repo.pool())
            .await
            .unwrap();
        assert!(repo.claim_parked(&terminal.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn claim_parked_fetch_misses_when_completion_wins_after_update() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let parked = parked_operation(&repo, now_ms() + 10_000).await;
        let now = now_ms();
        let lease_owner = new_id();
        let result = sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'
                 AND (lease_owner IS NULL OR lease_until_ms < ?3)"#,
        )
        .bind(&lease_owner)
        .bind(now + OPERATION_LEASE_MS)
        .bind(now)
        .bind(&parked.id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();
        assert_eq!(result.rows_affected(), 1);

        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        assert!(matches!(
            complete_parked_tx(
                &mut tx,
                &parked.id,
                &ParkedOutcome::Succeeded {
                    result: json!({ "winner": "completion" }),
                },
            )
            .await
            .unwrap(),
            ParkedCompletion::Completed(_)
        ));
        tx.commit().await.unwrap();

        assert!(
            fetch_claimed_parked(sqlx_repo.pool(), &parked.id, &lease_owner)
                .await
                .unwrap()
                .is_none(),
            "post-claim fetch must miss after completion clears the lease"
        );
    }

    #[tokio::test]
    async fn completion_clears_lease_so_claimed_deadline_write_loses_ordering_b() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
        let parked = parked_operation(&repo, now_ms() + 10_000).await;
        let claimed = repo.claim_parked(&parked.id).await.unwrap().unwrap();
        assert!(claimed.lease_owner.is_some());

        let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
        assert!(matches!(
            complete_parked_tx(
                &mut tx,
                &parked.id,
                &ParkedOutcome::Succeeded {
                    result: json!({ "winner": "completion" }),
                },
            )
            .await
            .unwrap(),
            ParkedCompletion::Completed(_)
        ));
        tx.commit().await.unwrap();

        assert!(
            repo.mark_failed(
                &claimed,
                "deadline".into(),
                PhaseTag::Parked,
                Some("parked_deadline".into()),
            )
            .await
            .unwrap()
            .is_none(),
            "completion cleared the claim lease so mark_failed cannot overwrite"
        );
        let result = repo.operation_result(&parked.id).await.unwrap().unwrap();
        assert!(matches!(
            result.outcome,
            OperationOutcome::Succeeded { ref result }
                if result == &json!({ "winner": "completion" })
        ));
    }

    #[tokio::test]
    async fn recover_on_boot_plan_contains_verify_parked_items() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let pool = sqlx_repo.pool().clone();
        let repo = Arc::new(SqlxOperationRepo::new(pool));
        let parked = parked_operation(repo.as_ref(), now_ms() + 10_000).await;
        let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(TestParkingAdapter {
            observer_runs,
            record_artifacts: true,
            steal_lease_after_artifacts: false,
        });
        let runtime = test_runtime(sqlx_repo, repo, vec![adapter]);

        let plan = runtime.recover_on_boot().await.unwrap();
        assert!(plan.items.iter().any(|item| {
            matches!(item, RecoveryItem::VerifyParked { op_id } if op_id == &parked.id)
        }));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn boot_leave_parked_clears_abandoned_future_lease() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let pool = sqlx_repo.pool().clone();
        let repo = Arc::new(SqlxOperationRepo::new(pool));
        let parked = parked_operation(repo.as_ref(), now_ms() + 10_000).await;
        let (_child, artifacts) = live_child_spawn_artifacts();
        let artifacts_json = serde_json::to_string(&artifacts).unwrap();
        let now = now_ms();
        sqlx::query(
            r#"UPDATE operations
               SET spawn_artifacts_json = ?1,
                   lease_owner = 'abandoned-boot-lease',
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'"#,
        )
        .bind(artifacts_json)
        .bind(now + OPERATION_LEASE_MS)
        .bind(now)
        .bind(&parked.id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();

        let before = repo.get_operation(&parked.id).await.unwrap().unwrap();
        assert_eq!(before.lease_owner.as_deref(), Some("abandoned-boot-lease"));
        assert!(before.lease_until_ms.is_some_and(|lease| lease > now_ms()));

        let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(TestParkingAdapter {
            observer_runs,
            record_artifacts: true,
            steal_lease_after_artifacts: false,
        });
        let runtime = test_runtime(sqlx_repo, repo.clone(), vec![adapter]);
        let plan = runtime.recover_on_boot().await.unwrap();

        runtime.apply_recovery(plan).await.unwrap();

        let stored = repo.get_operation(&parked.id).await.unwrap().unwrap();
        assert_eq!(stored.phase, Phase::Parked);
        assert!(stored.lease_owner.is_none());
        assert!(stored.lease_until_ms.is_none());

        let claimed = repo.claim_parked(&parked.id).await.unwrap();
        assert!(
            claimed.is_some(),
            "steady-state claim must not wait for the abandoned boot lease"
        );
    }

    #[tokio::test]
    async fn parked_return_without_artifacts_fails_and_drops_observer() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let pool = sqlx_repo.pool().clone();
        let repo = Arc::new(SqlxOperationRepo::new(pool));
        let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(TestParkingAdapter {
            observer_runs: observer_runs.clone(),
            record_artifacts: false,
            steal_lease_after_artifacts: false,
        });
        let runtime = test_runtime(sqlx_repo, repo, vec![adapter]);
        let op_id = runtime
            .submit(
                "park-test",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({ "wave_id": "wave-a" }),
            )
            .await
            .unwrap();

        let result = runtime.wait(&op_id).await.unwrap();
        assert!(matches!(
            result.outcome,
            OperationOutcome::Failed {
                from_phase: PhaseTag::SpawnStarted,
                ..
            }
        ));
        assert_eq!(
            observer_runs.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "observer must be dropped when set_parked fails the artifact fence"
        );
    }

    #[tokio::test]
    async fn set_parked_lost_lease_after_artifacts_drops_observer() {
        let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let pool = sqlx_repo.pool().clone();
        let repo = Arc::new(SqlxOperationRepo::new(pool));
        let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(TestParkingAdapter {
            observer_runs: observer_runs.clone(),
            record_artifacts: true,
            steal_lease_after_artifacts: true,
        });
        let runtime = test_runtime(sqlx_repo, repo.clone(), vec![adapter]);
        let op_id = runtime
            .submit(
                "park-test",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({ "wave_id": "wave-a" }),
            )
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
        assert_eq!(stored.phase, Phase::SpawnStarted);
        assert_eq!(stored.lease_owner.as_deref(), Some("stolen-driver"));
        assert!(stored.spawn_artifacts.is_some());
        assert_eq!(
            observer_runs.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "observer must be dropped when set_parked loses the lease"
        );
    }

    async fn claimed_spawn_started_operation(repo: &SqlxOperationRepo) -> Operation {
        let op_id = repo
            .insert_operation(
                "park-test",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({ "wave_id": "wave-a" }),
            )
            .await
            .unwrap();
        let now = now_ms();
        let lease_owner = new_id();
        sqlx::query(
            r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4"#,
        )
        .bind(&lease_owner)
        .bind(now + OPERATION_LEASE_MS)
        .bind(now)
        .bind(&op_id)
        .execute(&repo.pool)
        .await
        .unwrap();
        let output = TxOutput::new("unknown", None, json!({ "initial": true }));
        sqlx::query(
            r#"UPDATE operations
               SET phase = 'spawn_started',
                   tx_output_json = ?1,
                   target_type = ?2,
                   target_id = ?3,
                   target_json = ?4
               WHERE id = ?5"#,
        )
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&output.target_type)
        .bind(&output.target_id)
        .bind(
            serde_json::to_string(&json!({
                "type": output.target_type,
                "id": output.target_id,
            }))
            .unwrap(),
        )
        .bind(&op_id)
        .execute(&repo.pool)
        .await
        .unwrap();
        repo.get_operation(&op_id).await.unwrap().unwrap()
    }

    async fn parked_operation(repo: &SqlxOperationRepo, deadline_ms: TimestampMs) -> Operation {
        let op = claimed_spawn_started_operation(repo).await;
        repo.record_spawn_artifacts(&op, &sample_spawn_artifacts())
            .await
            .unwrap();
        repo.set_parked(&op, deadline_ms)
            .await
            .unwrap()
            .expect("operation parks")
    }

    fn sample_spawn_artifacts() -> SpawnArtifacts {
        SpawnArtifacts {
            pid: 1,
            pgid: 1,
            start_time: 1,
            boot_id: "boot".into(),
            log_path: None,
            extra: Value::Null,
        }
    }

    #[cfg(target_os = "linux")]
    struct ChildGuard(std::process::Child);

    #[cfg(target_os = "linux")]
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(target_os = "linux")]
    fn live_child_spawn_artifacts() -> (ChildGuard, SpawnArtifacts) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn live child");
        let pid = i32::try_from(child.id()).expect("child pid fits i32");
        let start_time = crate::proc_identity::read_proc_start_time(pid).expect("child start time");
        let boot_id = crate::proc_identity::read_boot_id().expect("current boot id");
        let artifacts = SpawnArtifacts {
            pid,
            pgid: pid,
            start_time,
            boot_id,
            log_path: None,
            extra: Value::Null,
        };
        assert!(parked_artifacts_alive(&artifacts));
        (ChildGuard(child), artifacts)
    }

    fn test_runtime(
        sqlx_repo: crate::db::sqlite::SqlxRepo,
        operation_repo: Arc<SqlxOperationRepo>,
        adapters: Vec<Arc<dyn ProviderAdapter>>,
    ) -> OperationRuntime {
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let route_repo: Arc<dyn crate::db::RouteRepo> = Arc::new(sqlx_repo);
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        OperationRuntime::new_unchecked(
            operation_repo.clone(),
            adapters,
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events,
                completion,
            ),
        )
    }

    struct TestParkingAdapter {
        observer_runs: Arc<std::sync::atomic::AtomicUsize>,
        record_artifacts: bool,
        steal_lease_after_artifacts: bool,
    }

    #[async_trait]
    impl ProviderAdapter for TestParkingAdapter {
        fn kind(&self) -> &'static str {
            "park-test"
        }

        fn phases(&self) -> &'static [PhaseTag] {
            &[
                PhaseTag::Pending,
                PhaseTag::TxCommitted,
                PhaseTag::SpawnStarted,
                PhaseTag::Parked,
                PhaseTag::Compensating,
                PhaseTag::Failed,
            ]
        }

        async fn validate(&self, _input: &Value) -> Result<()> {
            Ok(())
        }

        async fn prepare_tx<'tx>(
            &self,
            _tx: &mut Tx<'tx>,
            _input: &Value,
            _op: &Operation,
        ) -> Result<TxOutput> {
            Ok(TxOutput::new("unknown", None, json!({ "prepared": true })))
        }

        async fn app_server_interact(
            &self,
            _output: &mut TxOutput,
            _op: &Operation,
            _ctx: &SpawnCtx,
        ) -> Result<AppServerInteractOutcome> {
            Ok(AppServerInteractOutcome::NotApplicable)
        }

        async fn spawn_side_effect(
            &self,
            _output: &TxOutput,
            op: &Operation,
            ctx: &SpawnCtx,
        ) -> Result<SpawnOutcome> {
            if self.record_artifacts {
                ctx.record_spawn_artifacts(op, &sample_spawn_artifacts())
                    .await?;
                if self.steal_lease_after_artifacts {
                    let pool = ctx.operation_repo.sqlite_pool();
                    let now = now_ms();
                    let result = sqlx::query(
                        r#"UPDATE operations
                           SET lease_owner = 'stolen-driver',
                               lease_until_ms = ?1,
                               updated_at_ms = ?2
                           WHERE id = ?3
                             AND phase = 'spawn_started'
                             AND lease_owner = ?4"#,
                    )
                    .bind(now + OPERATION_LEASE_MS)
                    .bind(now)
                    .bind(&op.id)
                    .bind(required_lease_owner(op)?)
                    .execute(&pool)
                    .await?;
                    if result.rows_affected() == 0 {
                        return Err(CalmError::Internal(
                            "test adapter failed to steal operation lease".into(),
                        ));
                    }
                }
            }
            let observer_runs = self.observer_runs.clone();
            Ok(SpawnOutcome::Parked {
                deadline_ms: now_ms() + 10_000,
                observer: Box::pin(async move {
                    observer_runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            })
        }

        async fn plan_compensation(
            &self,
            from_phase: PhaseTag,
            reason: &str,
            _output: &TxOutput,
            _op: &Operation,
        ) -> Result<CompensationStateVersioned> {
            Ok(CompensationStateVersioned {
                version: 1,
                from_phase,
                reason: reason.into(),
                steps: Vec::new(),
            })
        }

        async fn compensate_step(
            &self,
            _step: &CompensationStep,
            _output: &TxOutput,
            _op: &Operation,
            _ctx: &SpawnCtx,
        ) -> Result<()> {
            Ok(())
        }
    }
}
