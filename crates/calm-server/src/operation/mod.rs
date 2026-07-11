#[cfg(test)]
mod parked_fence_model;

mod driver;
mod repo_sqlite;
pub(crate) mod workspace_lease;

pub mod claude_adapter;
pub mod claude_restart_adapter;
pub mod codex_adapter;
pub mod forge_action_adapter;
pub mod spec_harness_interrupt_adapter;
pub mod spec_harness_shutdown_adapter;
pub mod spec_harness_start_adapter;
pub mod task_verify_adapter;
pub mod terminal_adapter;
pub(crate) mod worker_cleanup;

pub use driver::{OperationCompletionBus, OperationRuntime};
pub use repo_sqlite::SqlxOperationRepo;
#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub use repo_sqlite::complete_parked_for_test;
pub(crate) use repo_sqlite::{checkpoint_app_server_interact_tx, complete_parked_tx};
use repo_sqlite::{fetch_claimed_parked, operation_from_row};

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, SqlitePool, Transaction};
#[cfg(test)]
use tokio::sync::Mutex;

#[cfg(test)]
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, EventBus};
use crate::model::{new_id, now_ms};
use crate::proc_identity::verify_owned_pid;
use crate::routes::terminal::spawn_terminal_with_parts;
use crate::shared_codex_appserver::SharedCodexAppServer;
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

    pub(crate) fn output_string(&self, key: &str, ctx: &str) -> Result<String> {
        self.data
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| CalmError::Internal(format!("{ctx} tx_output missing {key}")))
    }

    pub(crate) fn output_optional_string(&self, key: &str, ctx: &str) -> Result<Option<String>> {
        match self.data.get(key) {
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(Value::Null) | None => Ok(None),
            Some(_) => Err(CalmError::Internal(format!(
                "{ctx} tx_output {key} must be string or null"
            ))),
        }
    }

    pub(crate) fn set_output_data(&mut self, key: &str, value: Value, ctx: &str) -> Result<()> {
        let data = self
            .data
            .as_object_mut()
            .ok_or_else(|| CalmError::Internal(format!("{ctx} tx_output data is not an object")))?;
        data.insert(key.to_string(), value);
        Ok(())
    }

    pub(crate) fn non_empty_string(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
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
    pub shared_codex_appserver: Option<Arc<SharedCodexAppServer>>,
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
            shared_codex_appserver: None,
        }
    }

    pub fn with_shared_codex_appserver(mut self, shared: Arc<SharedCodexAppServer>) -> Self {
        self.shared_codex_appserver = Some(shared);
        self
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

impl CompensationStep {
    pub(crate) fn new(op: &str, args: Value) -> Self {
        Self {
            op: op.to_string(),
            args,
            completed: false,
            attempts: 0,
            last_error: None,
        }
    }

    pub(crate) fn arg_string(&self, key: &str, ctx: &str) -> Result<String> {
        self.args
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                CalmError::Internal(format!("{ctx} compensation step {} missing {key}", self.op))
            })
    }
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

fn required_lease_owner(op: &Operation) -> Result<&str> {
    op.lease_owner.as_deref().ok_or_else(|| {
        CalmError::Internal(format!(
            "operation {} is not claimed by the current driver",
            op.id
        ))
    })
}

fn parked_artifacts_alive(artifacts: &SpawnArtifacts) -> bool {
    verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id)
}

pub(crate) fn operation_result_from(op: &Operation) -> Result<Option<OperationResult>> {
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

fn required_output(op: &Operation) -> Result<&TxOutput> {
    op.tx_output
        .as_ref()
        .ok_or_else(|| CalmError::Internal(format!("operation {} missing tx_output_json", op.id)))
}

#[cfg(test)]
mod claim_completion_deadlock_tests;
#[cfg(test)]
mod tests;
