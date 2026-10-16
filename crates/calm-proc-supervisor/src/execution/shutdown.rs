use super::*;

impl Executor {
    /// Stop only native handles owned by this executor. Timeout never becomes drain evidence.
    pub async fn stop_known(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ids = match self.records.lock() {
            Ok(records) => records.keys().cloned().collect::<Vec<_>>(),
            Err(_) => return,
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let mut unfinished = false;
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
                if matches!(record.phase, Phase::Prepared) {
                    record.phase = Phase::Revoked;
                    let _ = self.persist(record);
                    continue;
                }
                if record.phase.state().permits_handoff() {
                    continue;
                }
                unfinished = true;
                let process = self
                    .processes
                    .lock()
                    .ok()
                    .and_then(|processes| processes.get(id).cloned());
                if let Some(process) = process {
                    let _ = process.signal(calm_session::control::ProcSignal::Term);
                }
            }
            if !unfinished || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
