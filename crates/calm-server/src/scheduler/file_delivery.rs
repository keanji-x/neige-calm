//! Publication waits never occupy the Track scheduling lock or worker budget.
use super::*;
use calm_types::task_execution::FileDelivery;
impl Scheduler {
    pub(super) fn drive_file_producers(self: &Arc<Self>, tasks: &[Task]) {
        for source in tasks.iter().filter(|task| task.status == TaskStatus::Done) {
            if !matches!(
                crate::file_delivery::selection(source),
                Ok(Some(FileDelivery::Producer { .. }))
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
        self.poke(source.track_id.clone().into());
        Ok(())
    }
}
