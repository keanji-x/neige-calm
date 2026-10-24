use super::*;
use std::time::Duration;

use calm_session::control::execution::{DRAIN_BUDGET, STOP_GRACE};

impl Executor {
    /// Caller holds the resource gate. The first Stop fixes the deadline for this native scope.
    pub(super) fn stop_execution(&self, id: &ExecutionId) -> anyhow::Result<()> {
        let process = self
            .processes
            .lock()
            .map_err(poison)?
            .get(id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("original native execution unavailable; recovery required")
            })?;
        anyhow::ensure!(
            process.ready.load(std::sync::atomic::Ordering::SeqCst),
            "helper is not ready for Stop"
        );
        let mut deadline = process.stop_deadline.lock().map_err(poison)?;
        let first = deadline.is_none();
        let kill_at = *deadline.get_or_insert(tokio::time::Instant::now() + STOP_GRACE);
        drop(deadline);
        #[cfg(target_os = "linux")]
        process.startup_changed.notify_waiters();
        if !first {
            return Ok(());
        }
        tokio::spawn(async move {
            if let Err(error) = process.signal(calm_session::control::ProcSignal::Term) {
                tracing::error!(%error, "cannot TERM execution scope");
            }
            tokio::time::sleep_until(kill_at).await;
            if let Err(error) = process.signal(calm_session::control::ProcSignal::Kill) {
                tracing::error!(%error, "cannot KILL execution scope");
            }
        });
        Ok(())
    }

    /// Close admission, use the lifecycle Stop routine, and await durable scope facts only.
    pub async fn graceful_shutdown(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let shutdown = async {
            let ids = match self.records.lock() {
                Ok(records) => records.keys().cloned().collect::<Vec<_>>(),
                Err(_) => return,
            };
            let mut waiting = Vec::new();
            for id in &ids {
                let Ok(first) = self.record(id) else {
                    continue;
                };
                let Ok(gate) = self.gate(scope_resource(&first.request.identity.scope)) else {
                    continue;
                };
                let _fence = gate.lock().await;
                let Ok(mut record) = self.record(id) else {
                    continue;
                };
                if matches!(record.phase, Phase::Prepared | Phase::Reserved { .. }) {
                    record.phase = match record.phase {
                        Phase::Reserved { slot_version } => Phase::NoChild {
                            slot_version: Some(slot_version),
                            reason: "executor closed before fork barrier".into(),
                        },
                        _ => Phase::Revoked,
                    };
                    record.interactive_revoked = true;
                    if let Err(error) = self.persist(record) {
                        tracing::error!(%error, "cannot persist shutdown before-fork settlement");
                    }
                } else if !record.durably_settled() {
                    let live = self
                        .processes
                        .lock()
                        .is_ok_and(|processes| processes.contains_key(id));
                    match self.stop_execution(id) {
                        Ok(()) => waiting.push(id.clone()),
                        Err(error) => {
                            tracing::error!(%error, "cannot stop execution during shutdown");
                            if live {
                                waiting.push(id.clone());
                            }
                        }
                    }
                }
            }
            loop {
                if waiting
                    .iter()
                    .all(|id| self.record(id).is_ok_and(|record| record.durably_settled()))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        if tokio::time::timeout(STOP_GRACE + DRAIN_BUDGET, shutdown)
            .await
            .is_err()
        {
            tracing::error!("executor shutdown timed out without durable settlement");
        }
        #[cfg(target_os = "macos")]
        if let Ok(processes) = self.processes.lock() {
            // Unknown lifecycle observations must not strand children whose
            // original ownership is still held when the supervisor exits.
            for process in processes.values() {
                if let Err(error) = process.cleanup() {
                    tracing::error!(%error, "native shutdown cleanup incomplete");
                }
            }
        }
    }
}
