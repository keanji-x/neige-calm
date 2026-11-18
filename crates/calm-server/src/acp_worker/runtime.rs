//! Task control state and callbacks supplied to the ACP host at application composition.
use super::{Workers, admission::Admission, lifecycle, session};
use crate::{
    acp_session::{host::AcpHost, roles::NativeWorkers},
    error::{CalmError, Result},
    operation::{Operation, SpawnCtx, TxOutput},
    provider_execution::{Claim, CompletionEvidence, KnownEnd, Purpose, Record},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[async_trait::async_trait]
impl NativeWorkers for Workers {
    fn active_turn(&self, session: &str) -> Option<Option<String>> {
        self.active_turn(session)
    }
    fn fence(&self, session: &str, turn: Option<&str>) -> bool {
        let Some(control) = self.get(session) else {
            return false;
        };
        if turn.is_some_and(|turn| control.turn != turn) {
            return false;
        }
        control.authorized.store(false, Ordering::Release);
        let _ = control.cancel.send(super::CancelRequest::Requested);
        true
    }
    async fn start(
        self: Arc<Self>,
        host: Arc<AcpHost>,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        let admission = Admission {
            repo: ctx.repo.clone(),
            worker: output.output_string("runtime_id", "ACP Worker")?,
            card: output.output_string("card_id", "ACP Worker")?,
            track: output.output_string("track_id", "ACP Worker")?,
            attempt: output.output_string("attempt_id", "ACP Worker")?,
            operation: op.id.clone(),
            lease: output.output_string("lease_id", "ACP Worker")?,
            cwd: PathBuf::from(output.output_string("cwd", "ACP Worker")?),
            authorized: Arc::new(AtomicBool::new(true)),
            native: None,
            credential: None,
            turn: None,
        };
        session::start(
            host,
            self,
            admission,
            output.output_string("prompt", "ACP Worker")?,
            ctx.clone(),
        )
        .await
    }
    async fn stop(&self, host: &AcpHost, session: &str, turn: Option<&str>) -> Result<()> {
        lifecycle::stop_session(host, self, session, turn).await
    }
    async fn stop_if_owned(&self, host: &AcpHost, session: &str) -> Result<bool> {
        let pool = host
            .completion
            .repo()?
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE id=?1 AND json_extract(handle_state_json,'$.requested_selection.provider') IS NOT NULL)")
            .bind(session).fetch_one(pool.read_pool()).await?;
        if owned {
            self.stop(host, session, None).await?;
        }
        Ok(owned)
    }
    async fn revoke_owned_tx(
        &self,
        tx: &mut crate::operation::Tx<'_>,
        track: Option<&str>,
    ) -> Result<Vec<String>> {
        let workers:Vec<String>=sqlx::query_scalar("SELECT id FROM worker_sessions WHERE json_extract(handle_state_json,'$.requested_selection.provider') IS NOT NULL AND (?1 IS NULL OR track_id=?1)").bind(track).fetch_all(&mut **tx).await?;
        for worker in &workers {
            sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
                .bind(worker)
                .execute(&mut **tx)
                .await?;
        }
        Ok(workers)
    }
    async fn recover(&self, repo: &dyn crate::db::Repo, confirmed: bool) -> Result<()> {
        lifecycle::recover_after_boot(repo, confirmed).await
    }
    async fn reconcile(&self, host: &AcpHost, session: &str, claim: &Claim) -> Result<()> {
        let Purpose::WorkerTurn { turn_id, .. } = &claim.record().purpose else {
            return Err(CalmError::Conflict("wrong native Worker purpose".into()));
        };
        if self
            .get(session)
            .is_some_and(|control| control.settled.has_changed().is_err())
        {
            self.stop(host, session, Some(turn_id)).await?;
        }
        Ok(())
    }
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        _: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> Result<()> {
        match known {
            Some(KnownEnd::AcpWorkerTurn(end)) => lifecycle::complete(session, record, *end).await,
            _ => Err(CalmError::Conflict(
                "native Worker completion requires its captured round evidence".into(),
            )),
        }
    }
}
