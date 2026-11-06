//! Original-group observations are produced by the owning supervisor, never by a helper report.
use super::*;

impl Executor {
    pub(super) fn monitor_native(
        self: &Arc<Self>,
        id: ExecutionId,
        process: Arc<process::Process>,
    ) {
        let executor = self.clone();
        tokio::spawn(async move {
            let result = executor.observe_native(&id, &process).await;
            if let Err(error) = result {
                if let Err(cleanup) = process.cleanup() {
                    tracing::error!(%cleanup, "native failure cleanup incomplete");
                }
                let first = executor.record(&id)?;
                let gate = executor.gate(scope_resource(&first.request.identity.scope))?;
                let _fence = gate.lock().await;
                let mut record = executor.record(&id)?;
                record.phase = Phase::Unknown {
                    slot_version: record
                        .phase
                        .slot_version()
                        .ok_or_else(|| anyhow::anyhow!("missing native slot"))?,
                    receipt: record.phase.receipt().cloned(),
                    reason: format!("native original-group completion unknown: {error}"),
                };
                executor.persist(record)?;
            }
            Ok::<(), anyhow::Error>(())
        });
    }
    async fn observe_native(
        &self,
        id: &ExecutionId,
        process: &Arc<process::Process>,
    ) -> anyhow::Result<()> {
        loop {
            match process.poll()? {
                process::Observation::Running => {}
                process::Observation::Exited { status, empty } => {
                    let first = self.record(id)?;
                    let gate = self.gate(scope_resource(&first.request.identity.scope))?;
                    {
                        let _fence = gate.lock().await;
                        let mut record = self.record(id)?;
                        let receipt = record
                            .phase
                            .receipt()
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("missing native receipt"))?;
                        if !matches!(record.phase, Phase::Exited { .. }) {
                            record.phase = Phase::Exited { receipt, status };
                            self.persist(record)?;
                        }
                        if !empty {
                            self.stop_execution(id)?;
                        }
                    }
                    if empty {
                        process.reap()?;
                        let log = self
                            .outputs
                            .lock()
                            .map_err(poison)?
                            .get(id)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("native output log missing"))?;
                        let output_end = log.finish_bounded(DRAIN_BUDGET).await;
                        let _fence = gate.lock().await;
                        let mut record = self.record(id)?;
                        record.output_end = output_end;
                        let receipt = record
                            .phase
                            .receipt()
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("missing native receipt"))?;
                        record.phase = if matches!(record.output_end, OutputEnd::Complete { .. }) {
                            Phase::OriginalGroupEmpty { receipt, status }
                        } else {
                            Phase::Unknown {
                                slot_version: receipt.slot_version,
                                receipt: Some(receipt),
                                reason: "native original group empty but output incomplete".into(),
                            }
                        };
                        let complete = matches!(record.output_end, OutputEnd::Complete { .. });
                        self.persist(record)?;
                        log.close_writer()?;
                        // The original group was observed empty and this exact child was reaped.
                        // Incomplete output remains Unknown, retaining its native owner as before.
                        if complete {
                            self.processes.lock().map_err(poison)?.remove(id);
                        }
                        return Ok(());
                    }
                }
            }
            if let Some(kill_at) = *process.stop_deadline.lock().map_err(poison)? {
                anyhow::ensure!(
                    tokio::time::Instant::now() < kill_at + DRAIN_BUDGET,
                    "native original group drain budget exhausted"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
