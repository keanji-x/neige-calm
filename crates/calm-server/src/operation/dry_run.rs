//! A rolled-back run of an operation's real prepare (#2516): what a submit would refuse, asked
//! before a caller does something a refusal could not undo.

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::{Operation, OperationRuntime, Phase, Tx};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;

impl OperationRuntime {
    /// Run `kind`'s validate and prepare for `payload` on the state `setup` writes first, all in
    /// one transaction that is rolled back: nothing commits, and the prepare's post-commit events
    /// are dropped unsent. The refusal is the prepare's own, so no caller restates its rules.
    pub(crate) async fn dry_run_prepare<F>(
        &self,
        kind: &str,
        payload: &Value,
        setup: F,
    ) -> Result<()>
    where
        F: for<'a, 'tx> FnOnce(&'a mut Tx<'tx>) -> BoxFuture<'a, Result<()>>,
    {
        let adapter = self.adapter(kind)?;
        adapter.validate(payload).await?;
        let pool = self.repo.sqlite_pool();
        let mut tx = begin_immediate_tx(&pool).await?;
        let verdict = match setup(&mut tx).await {
            Ok(()) => adapter
                .prepare_tx(&mut tx, payload, &dry_run_operation(kind, payload))
                .await
                .map(|_prepared| ()),
            Err(error) => Err(error),
        };
        tx.rollback().await?;
        verdict
    }
}

/// The unsaved operation a dry run hands the prepare.
fn dry_run_operation(kind: &str, payload: &Value) -> Operation {
    Operation {
        id: "dry-run".into(),
        operation_key: "dry-run".into(),
        kind: kind.to_string(),
        idempotency_key: None,
        payload_hash: String::new(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({ "type": "unknown", "id": null }),
        payload: payload.clone(),
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
