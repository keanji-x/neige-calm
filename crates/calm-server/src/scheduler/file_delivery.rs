//! Publication waits never occupy the Track scheduling lock or worker budget.
use super::*;
use crate::db::write_in_tx_typed;
use calm_types::task_execution::FileDelivery;
impl Scheduler {
    pub(super) fn drive_file_producers(self: &Arc<Self>, tasks: &[Task]) {
        for source in tasks.iter().filter(|task| task.status == TaskStatus::Done) {
            if !matches!(
                crate::file_delivery::selection(source),
                Ok(Some(
                    FileDelivery::Producer { .. } | FileDelivery::CandidateProducer { .. }
                ))
            ) {
                continue;
            }
            let key = format!("file:{}", source.id);
            let Some(guard) = InflightGuard::acquire(&self.inflight, &key) else {
                continue;
            };
            let source = source.clone();
            let this = self.clone();
            tokio::spawn(async move {
                let _guard = guard;
                if let Err(error) = this.drive_file_source(&source).await {
                    tracing::debug!(task_id=%source.id, %error, "file publication is not eligible");
                }
            });
        }
    }
    async fn drive_file_source(self: &Arc<Self>, source: &Task) -> Result<()> {
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return Ok(());
        };
        let kind = crate::file_delivery::OPERATION_KIND;
        let key = format!("file:{}", source.id);
        // A settled failure is durable, never implicitly retried or turned into a new task.
        if let Some(publication) = runtime.find_by_kind_and_idempotency(kind, &key).await? {
            if runtime.operation_result(&publication.id).await?.is_none() {
                runtime.wait(&publication.id).await?;
                self.poke(source.track_id.clone().into());
            }
            crate::file_delivery::settlement::record(
                self.repo.as_ref(),
                &self.events,
                &self.write,
                &publication.id,
            )
            .await?;
            self.drive_candidate(source, &publication.id).await?;
            return Ok(());
        }
        let Some(worker) = runtime
            .find_by_kind_and_idempotency(crate::isolated_codex::OPERATION_KIND, &source.id)
            .await?
        else {
            return Ok(());
        };
        let result = runtime.wait(&worker.id).await?;
        if !matches!(result.outcome, OperationOutcome::Succeeded { .. }) {
            return Ok(());
        }
        let payload = serde_json::to_value(crate::file_delivery::PublicationPayload {
            task_id: source.id.clone(),
            track_id: source.track_id.clone(),
            source_operation_id: worker.id,
        })?;
        let id = runtime
            .submit(
                kind,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(key),
                    payload_hash: stable_payload_hash(&payload)?,
                },
                payload,
            )
            .await?;
        runtime.wait(&id).await?;
        crate::file_delivery::settlement::record(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            &id,
        )
        .await?;
        self.drive_candidate(source, &id).await?;
        self.poke(source.track_id.clone().into());
        Ok(())
    }
    async fn drive_candidate(self: &Arc<Self>, source: &Task, publication: &str) -> Result<()> {
        if !matches!(
            crate::file_delivery::selection(source)?,
            Some(FileDelivery::CandidateProducer { .. })
        ) {
            return Ok(());
        }
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return Ok(());
        };
        let kind = crate::file_delivery::candidate_verify::KIND;
        let key = format!("candidate:{publication}");
        let publication_id = publication.to_owned();
        let fallback = self.budget_default;
        let global_limit = self.candidate_verification_limit as i64;
        let operation_key = write_in_tx_typed(self.repo.as_ref(),move |tx| Box::pin(async move {
            let candidate = crate::file_delivery::candidate::load_tx(tx,&publication_id).await?;
            if let Some(key) = sqlx::query_scalar::<_,String>("SELECT operation_key FROM task_candidate_verification_allocations WHERE publication_operation_id=?1").bind(&publication_id).fetch_optional(&mut **tx).await? { return Ok(Some(key)); }
            crate::file_delivery::candidate::authorize_tx(tx,&candidate).await?;
            let (_,budget) = track_lifecycle_and_budget_tx(tx,&candidate.source.track_id).await?.ok_or_else(|| CalmError::Conflict("candidate track missing".into()))?;
            let configured: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key=?1").bind(crate::routes::settings::TASK_BUDGET_DEFAULT_KEY).fetch_optional(&mut **tx).await?;
            let budget = budget.unwrap_or(crate::routes::settings::effective_task_budget_default(configured.as_deref(),fallback)).max(0);
            let tasks = tasks_by_track_tx(tx,&candidate.source.track_id).await?;
            let active = crate::file_delivery::candidate_verify::active_tx(tx,&candidate.source.track_id).await?;
            let global: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE o.id IS NULL OR o.phase NOT IN ('succeeded','failed')").fetch_one(&mut **tx).await?;
            if track_capacity(&tasks,budget) as i64 <= active || global >= global_limit { return Ok(None); }
            let key = new_id();
            sqlx::query("INSERT INTO task_candidate_verification_allocations(publication_operation_id,track_id,operation_key) VALUES(?1,?2,?3)").bind(&publication_id).bind(&candidate.source.track_id).bind(&key).execute(&mut **tx).await?;
            Ok(Some(key))
        })).await?;
        let Some(operation_key) = operation_key else {
            return Ok(());
        };
        let id = if let Some(op) = runtime.find_by_kind_and_idempotency(kind, &key).await? {
            op.id
        } else {
            let payload = serde_json::json!({"publication_operation_id":publication});
            let _permit = self
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| CalmError::Conflict("dispatcher semaphore closed".into()))?;
            runtime
                .submit(
                    kind,
                    OperationKey {
                        operation_key,
                        idempotency_key: Some(key),
                        payload_hash: stable_payload_hash(&payload)?,
                    },
                    payload,
                )
                .await?
        };
        runtime.wait(&id).await?;
        crate::file_delivery::verification_settlement::record(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            &id,
        )
        .await?;
        self.poke(source.track_id.clone().into());
        Ok(())
    }
}
