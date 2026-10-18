//! Terminal's tx-only consumer of the execution engine. No Legacy spawn or compensation.
use async_trait::async_trait;
use serde_json::Value;
use workspace_runtime::execution::*;

use super::terminal_adapter::TerminalAdapter;
use super::{Operation, ProviderAdapter, Tx, TxOnlyAdapter, TxOutput};
use crate::{
    error::{CalmError, Result},
    execution_quiesce::ExecutionLifecycle,
};

pub const CONSUMER: &str = "terminal";
pub const KIND: &str = "terminal-execution-create";

pub struct TerminalExecutionCreate {
    terminal: TerminalAdapter,
    execution: ExecutionLifecycle,
}
impl TerminalExecutionCreate {
    pub fn new(terminal: TerminalAdapter, execution: ExecutionLifecycle) -> Self {
        Self {
            terminal,
            execution,
        }
    }
    fn engine(&self) -> Result<&crate::execution_boot::ExecutionEngine> {
        self.execution
            .engine()
            .filter(|engine| engine.available())
            .ok_or(CalmError::ExecutionUnavailable)
    }
}
#[async_trait]
impl TxOnlyAdapter for TerminalExecutionCreate {
    fn kind(&self) -> &'static str {
        KIND
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        self.terminal.validate(input).await
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let engine = self.engine()?;
        // The Terminal owner supplies the same cwd chooser, domain rows and events as Legacy.
        let mut output = self.terminal.prepare_tx(tx, input, op).await?;
        let id = ExecutionId::fresh()?;
        let terminal_id = output.output_string("terminal_id", KIND)?;
        let program = output.output_string("program", KIND)?;
        let cwd = output.output_string("cwd", KIND)?;
        let env = &output.data["env"];
        let intent = ExecutionIntent {
            metadata: ExecutionMetadata {
                consumer: CONSUMER.into(),
                id,
                alias: ProcessAlias(calm_truth::db::sqlite::workspace_execution::terminal_alias(
                    &terminal_id,
                )),
                owner: OperationRef(op.id.clone()),
                track: TrackId(output.output_string("track_id", KIND)?),
                source: PrivateSource {
                    root: cwd.into(),
                    cwd: workspace::identity::WorkspaceCwd::root(),
                    registration: ObjectRegistrationId::fresh()?,
                    nonce: TransferNonce::fresh()?,
                    directory: ExternalDirectoryId::fresh()?,
                    incarnation: ExternalDirectoryIncarnationId::fresh()?,
                    authority: ExecutionAuthority {
                        grant_id: ExecutionGrantId::fresh()?,
                        authorization_binding: AuthorizationBindingId::fresh()?,
                        authorization_version: 1,
                    },
                },
                program: crate::routes::terminal::launch_specification(
                    &program,
                    env,
                    &crate::kernel_bin_path::shell_path_prepend()
                        .map_err(|e| CalmError::Internal(e.to_string()))?,
                ),
                specification: SpecificationId::fresh()?,
                ticket_id: LaunchTicketId::fresh()?,
            },
            secrets: ExecutionSecrets {
                ticket: CapabilityToken::fresh()?,
                tokens: ExecutionControlTokens::fresh()?,
            },
        };
        let ticket = engine
            .admit(tx, intent)
            .await
            .map_err(|e| CalmError::Internal(e.0))?;
        output.data["execution_id"] = serde_json::to_value(&ticket.0)?;
        output.result["execution_id"] = serde_json::to_value(&ticket.0)?;
        Ok(output)
    }
}

/// Endpoint identity spans both Terminal implementations. Typed lookup and admission share the
/// SQLite write lock; Legacy creation retains the ordinary submit and UNIQUE-index backstop.
pub async fn create(
    runtime: &super::OperationRuntime,
    events: &crate::event::EventBus,
    typed: bool,
    key: super::OperationKey,
    payload: Value,
) -> Result<super::OperationResult> {
    use super::repo_sqlite::{
        PhaseWrite, insert_pending_row, operation_from_row, write_phase_and_tx_output,
    };
    let pool = runtime.repo.sqlite_db();
    let mut validated = false;
    let mut tx = loop {
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        if let Some(idempotency_key) = &key.idempotency_key {
            let existing: Option<(String, String)> = sqlx::query_as(
                "SELECT id, payload_hash FROM operations WHERE kind IN ('terminal-create', \
                 'terminal-execution-create') AND idempotency_key = ?",
            )
            .bind(idempotency_key)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some((id, hash)) = existing {
                if hash != key.payload_hash {
                    return Err(super::idempotency_payload_conflict(Some(idempotency_key)));
                }
                tx.rollback().await?;
                return runtime.wait(&id).await;
            }
        }
        if !typed {
            // Keep Legacy's validate/insert/drive path, including concurrent keyed inserts.
            // The endpoint lookup above also permits replay of a typed row after config-off.
            tx.rollback().await?;
            let id = runtime.submit("terminal-create", key, payload).await?;
            return runtime.wait(&id).await;
        }
        if validated {
            break tx;
        }
        // A replay/conflict needs no new-request validation. Shared Terminal validation reads
        // through the repository, so release the transaction before borrowing its connection.
        // Recheck the key under the final write lock after validation to close that race.
        tx.rollback().await?;
        runtime
            .adapter("terminal-create")?
            .validate(&payload)
            .await?;
        validated = true;
    };
    let adapter = runtime.adapter(KIND)?;
    let id = crate::model::new_id();
    insert_pending_row(&id, KIND, &key, &payload)?
        .execute(&mut **tx)
        .await?;
    let row = sqlx::query("SELECT * FROM operations WHERE id = ?")
        .bind(&id)
        .fetch_one(&mut **tx)
        .await?;
    let op = operation_from_row(&row)?;
    let mut output = adapter.prepare_tx(&mut tx, &payload, &op).await?;
    let envelopes = std::mem::take(&mut output.post_commit_events);
    write_phase_and_tx_output(
        &mut tx,
        PhaseWrite::Inserted(&op),
        &super::Phase::Succeeded,
        &output,
    )
    .await?;
    tx.commit().await?;
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    runtime.wait(&id).await
}

/// Both fresh commits and replays carry the durable execution identity in their result.
pub(crate) fn drive_result(
    execution: &ExecutionLifecycle,
    result: &super::OperationResult,
) -> Result<()> {
    if let super::OperationOutcome::Succeeded { result }
    | super::OperationOutcome::SucceededViaCollision { result, .. } = &result.outcome
        && let Some(id) = result.get("execution_id")
    {
        execution.drive([serde_json::from_value::<ExecutionId>(id.clone())?]);
    }
    Ok(())
}

pub struct TerminalProjection;
#[async_trait]
impl crate::execution_store::ExecutionProjection for TerminalProjection {
    async fn spawned(
        &self,
        tx: &mut Tx<'_>,
        alias: &str,
        now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        calm_truth::db::sqlite::workspace_execution::terminal_spawned_tx(tx, alias, now).await
    }
    async fn settled(
        &self,
        tx: &mut Tx<'_>,
        alias: &str,
        settlement: &Settlement,
        now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        let exit = match settlement {
            Settlement::Journal(ExecutionState::Drained { status, .. }) => *status,
            _ => None,
        };
        calm_truth::db::sqlite::workspace_execution::terminal_settled_tx(tx, alias, exit, now).await
    }
}
