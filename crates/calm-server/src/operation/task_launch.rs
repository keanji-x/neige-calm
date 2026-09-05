//! Final recovery admission is serialized with withdrawal at controlled launch.
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use calm_types::task_recovery::TaskAttemptOrigin;
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

// Bound lock acquisition inside provider transports and socket writes too, not
// only their reply timers. Stay below the renewed 60-second operation lease.
const CONTROL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(45);

#[cfg(test)]
pub(crate) mod test_timeout {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{LazyLock, Mutex},
    };
    static VALUES: LazyLock<Mutex<HashMap<String, Duration>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    pub(crate) struct Override(String);
    pub(crate) fn install(task_id: &str, duration: Duration) -> Override {
        assert!(
            VALUES
                .lock()
                .unwrap()
                .insert(task_id.into(), duration)
                .is_none()
        );
        Override(task_id.into())
    }
    pub(super) fn read(task_id: &str) -> Duration {
        VALUES
            .lock()
            .unwrap()
            .get(task_id)
            .copied()
            .unwrap_or(CONTROL_EXCHANGE_TIMEOUT)
    }
    impl Drop for Override {
        fn drop(&mut self) {
            VALUES.lock().unwrap().remove(&self.0);
        }
    }
}

#[derive(Clone)]
pub(crate) struct TaskLaunch {
    task_id: String,
    operation: super::Operation,
}

#[derive(Debug)]
pub(crate) struct LaunchFailure<T> {
    pub error: CalmError,
    pub observed: Option<T>,
    pub effect_started: bool,
}
impl<T> From<CalmError> for LaunchFailure<T> {
    fn from(error: CalmError) -> Self {
        Self {
            error,
            observed: None,
            effect_started: false,
        }
    }
}

impl TaskLaunch {
    pub(crate) fn new(task_id: &str, operation: &super::Operation) -> Self {
        Self {
            task_id: task_id.into(),
            operation: operation.clone(),
        }
    }

    pub(crate) fn operation(&self) -> &super::Operation {
        &self.operation
    }

    pub(crate) async fn run<T, F>(self, repo: &dyn RepoEventWrite, effect: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        self.run_observed(repo, effect)
            .await
            .map_err(|failure| failure.error)
    }

    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    /// `effect` must contain only the bounded provider-control exchange, never
    /// repository writes. The write transaction remains held through that send
    /// and acknowledgement; PID/session persistence happens after it returns.
    /// Thus withdrawal committed before this controlled launch is seen by the
    /// final read, and concurrent withdrawal commits after launch admission.
    pub(crate) async fn run_observed<T, F>(
        self,
        repo: &dyn RepoEventWrite,
        effect: F,
    ) -> std::result::Result<T, LaunchFailure<T>>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let task_id = self.task_id.clone();
        let recovered = write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                crate::task_recovery::require_attempt_startable_tx(tx, &task_id).await?;
                let allocation = crate::db::sqlite::task_attempt_get_tx(tx, &task_id)
                    .await?
                    .ok_or_else(|| CalmError::Conflict("launch allocation is missing".into()))?;
                Ok(matches!(
                    allocation.origin,
                    TaskAttemptOrigin::Recovery { .. }
                ))
            })
        })
        .await?;
        // Keep the established already-prepared initial-attempt reconciliation
        // contract. Current/terminal execution identity is still fenced above.
        if !recovered {
            return effect.await.map_err(|error| LaunchFailure {
                error,
                observed: None,
                effect_started: true,
            });
        }
        let observed = Arc::new(Mutex::new(None));
        let captured = observed.clone();
        let started = Arc::new(AtomicBool::new(false));
        let started_in_tx = started.clone();
        let committed = write_in_tx_typed(repo, move |tx| Box::pin(async move {
            crate::task_recovery::require_attempt_startable_tx(tx, &self.task_id).await?;
            let owner = self.operation.lease_owner.as_deref()
                .ok_or_else(|| CalmError::Conflict("recovery launch requires an owned operation lease".into()))?;
            let now = crate::model::now_ms();
            let admission = serde_json::json!({"version":1,"task_id":self.task_id,"admitted_at_ms":now});
            // Preparation/phase transitions already persist the exact target.
            // Record this final admission in the existing mutable output before
            // sending the launch, without inventing another scheduling state.
            // Operation lease expiry permits takeover; ownership changes only
            // on that claim. Match the existing phase/artifact CAS contract:
            // the same owner may renew here, a claimed replacement cannot.
            let changed = sqlx::query(r#"
                UPDATE operations SET
                    tx_output_json=json_set(tx_output_json,'$.data.launch_admission',json(?1)),
                    updated_at_ms=?2, lease_until_ms=?6
                WHERE id=?3 AND lease_owner=?4 AND phase='spawn_started'
                  AND json_type(tx_output_json,'$.data')='object'
                  AND (
                    (kind IN ('codex-worker','claude-worker','terminal-worker')
                     AND idempotency_key=?5 AND json_extract(payload_json,'$.idempotency_key')=?5
                     AND target_type='card'
                     AND EXISTS(SELECT 1 FROM cards c WHERE c.id=operations.target_id AND c.role='worker'
                                AND c.track_id=(SELECT track_id FROM tasks WHERE id=?5)))
                    OR (kind='task-verify' AND json_extract(payload_json,'$.task_id')=?5
                        AND json_extract(payload_json,'$.attempt')=(SELECT gate_attempt FROM tasks WHERE id=?5)
                        AND target_type='task' AND target_id=?5)
                  )
            "#).bind(admission.to_string()).bind(now).bind(&self.operation.id).bind(owner).bind(&self.task_id)
                .bind(now.saturating_add(super::OPERATION_LEASE_MS))
                .execute(&mut **tx).await?.rows_affected();
            if changed != 1 { return Err(CalmError::Conflict("recovery launch operation binding or lease changed".into())); }
            #[cfg(test)]
            let control_timeout = test_timeout::read(&self.task_id);
            #[cfg(not(test))]
            let control_timeout = CONTROL_EXCHANGE_TIMEOUT;
            started_in_tx.store(true, Ordering::SeqCst);
            let output = tokio::time::timeout(control_timeout, effect).await.map_err(|_| {
                CalmError::Internal("recovery launch control exchange timed out; reconcile the prepared operation before another recovery".into())
            })??;
            *captured.lock().map_err(|_| CalmError::Internal("launch observation poisoned".into()))? = Some(output);
            Ok(())
        })).await;
        let observed = observed
            .lock()
            .map_err(|_| CalmError::Internal("launch observation poisoned".into()))?
            .take();
        match (committed, observed) {
            (Ok(()), Some(output)) => Ok(output),
            (Err(error), observed) => Err(LaunchFailure {
                error,
                observed,
                effect_started: started.load(Ordering::SeqCst),
            }),
            (Ok(()), None) => {
                Err(CalmError::Internal("launch completed without an observation".into()).into())
            }
        }
    }
}
