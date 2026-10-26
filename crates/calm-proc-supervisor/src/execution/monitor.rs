use super::*;

impl Executor {
    pub(super) fn monitor(
        self: &Arc<Self>,
        id: ExecutionId,
        reports: std::fs::File,
        process: Arc<process::Process>,
    ) {
        self.monitor_with_reap_barrier(id, reports, process, std::future::ready(()));
    }

    pub(super) fn monitor_with_reap_barrier(
        self: &Arc<Self>,
        id: ExecutionId,
        reports: std::fs::File,
        process: Arc<process::Process>,
        barrier: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || scope_helper::reports(reports, send));
        let executor = self.clone();
        tokio::spawn(async move {
            let mut drained = false;
            while let Some(event) = receive.recv().await {
                let outcome = executor.apply_event(&id, event).await;
                process.startup_changed.notify_waiters();
                match outcome {
                    Ok(true) => {
                        drained = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::error!(%error, "cannot persist execution event");
                        break;
                    }
                }
            }
            process
                .reports_ended
                .store(true, std::sync::atomic::Ordering::SeqCst);
            process.startup_changed.notify_waiters();
            if !drained {
                let _ = executor
                    .apply_event(
                        &id,
                        scope_helper::HelperReport {
                            context: {
                                let record = executor.record(&id)?;
                                let receipt = record
                                    .phase
                                    .receipt()
                                    .ok_or_else(|| anyhow::anyhow!("missing original receipt"))?;
                                scope_helper::HelperContext {
                                    execution_id: id.clone(),
                                    executor_boot: receipt.identity.executor_boot.clone(),
                                    nonce: receipt.process.nonce().clone(),
                                    mechanism: receipt.scope_mechanism.clone(),
                                }
                            },
                            event: scope_helper::HelperEvent::Unknown {
                                reason: "helper report stream ended without drain proof".into(),
                            },
                        },
                    )
                    .await;
            }
            barrier.await;
            let (reaped, wait) = tokio::sync::oneshot::channel();
            // A detached native wait must not keep Tokio shutdown alive if scope facts are Unknown.
            std::thread::spawn(move || {
                let result = process
                    .child
                    .lock()
                    .map_err(poison)
                    .and_then(|mut child| child.wait().map_err(anyhow::Error::from));
                process
                    .reaped
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = reaped.send(result);
            });
            wait.await??;
            if drained {
                let log = executor.outputs.lock().map_err(poison)?.get(&id).cloned();
                if let Some(log) = log {
                    let end = log.finish().await;
                    let first = executor.record(&id)?;
                    let gate = executor.gate(scope_resource(&first.request.identity.scope))?;
                    let _fence = gate.lock().await;
                    let mut record = executor.record(&id)?;
                    record.output_end = end;
                    executor.persist(record)?;
                }
            }
            Ok::<(), anyhow::Error>(())
        });
    }

    pub(super) async fn apply_event(
        &self,
        id: &ExecutionId,
        report: scope_helper::HelperReport,
    ) -> anyhow::Result<bool> {
        let first = self.record(id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        let _fence = gate.lock().await;
        let mut record = self.record(id)?;
        let receipt = record
            .phase
            .receipt()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("helper event lacks original process receipt"))?;
        anyhow::ensure!(
            report.context.execution_id == *id
                && report.context.executor_boot == receipt.identity.executor_boot
                && report.context.nonce.matches(receipt.process.nonce())
                && report.context.mechanism == receipt.scope_mechanism,
            "helper report identity mismatch"
        );
        // Durable terminal evidence is absorbing, including late lost-monitor reports.
        if record.phase.state().permits_handoff() {
            return Ok(matches!(record.phase, Phase::Drained { .. }));
        }
        record.phase = match report.event {
            scope_helper::HelperEvent::Ready => anyhow::bail!("duplicate helper readiness report"),
            scope_helper::HelperEvent::Started => {
                if let Some(process) = self.processes.lock().map_err(poison)?.get(id) {
                    process
                        .started
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                return Ok(false);
            }
            scope_helper::HelperEvent::LeaderExited { status } => Phase::Exited { receipt, status },
            scope_helper::HelperEvent::Drained { status } => {
                let proof = ScopeDrainProof {
                    helper_process: receipt.process.clone(),
                    mechanism: receipt.scope_mechanism.clone(),
                    kernel_state: KernelChildState::NoChildrenAfterForkEntryClosed,
                };
                Phase::Drained {
                    receipt,
                    status,
                    proof,
                }
            }
            scope_helper::HelperEvent::Unknown { reason } => Phase::Unknown {
                slot_version: receipt.slot_version,
                receipt: Some(receipt),
                reason,
            },
        };
        let drained = matches!(record.phase, Phase::Drained { .. });
        self.persist(record)?;
        Ok(drained)
    }
}
