use super::*;
use tokio::sync::broadcast;

pub(super) struct Changes {
    seq: u64,
    send: broadcast::Sender<(u64, ExecutionId)>,
}
impl Default for Changes {
    fn default() -> Self {
        Self {
            seq: 0,
            send: broadcast::channel(256).0,
        }
    }
}

impl Executor {
    pub(super) fn id_gate(&self, id: &ExecutionId) -> anyhow::Result<Arc<Mutex<()>>> {
        Ok(self
            .id_gates
            .lock()
            .map_err(poison)?
            .entry(id.clone())
            .or_default()
            .clone())
    }
    pub(super) fn changed(&self, id: ExecutionId) -> anyhow::Result<()> {
        let mut changes = self.changes.lock().map_err(poison)?;
        changes.seq = changes
            .seq
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("change sequence overflow"))?;
        let _ = changes.send.send((changes.seq, id));
        Ok(())
    }
    pub(super) fn watch(
        &self,
    ) -> anyhow::Result<(ExecutionReply, broadcast::Receiver<(u64, ExecutionId)>)> {
        let changes = self.changes.lock().map_err(poison)?;
        Ok((
            ExecutionReply::Watching {
                boot: self.boot.clone(),
                seq: changes.seq,
            },
            changes.send.subscribe(),
        ))
    }
    pub(super) fn inspect(&self, id: &ExecutionId) -> anyhow::Result<ExecutionReply> {
        if let Some(tombstone) = self.tombstones.lock().map_err(poison)?.get(id) {
            self.store.remove_execution(id)?;
            return Ok(ExecutionReply::Retired(tombstone.clone()));
        }
        Ok(match self.records.lock().map_err(poison)?.get(id) {
            // Latched IO failures can contain a terminal phase that is not yet a durable fact.
            // Inspect never repairs the journal or acknowledges that pending settlement.
            Some(record) if record.pending_persistence => {
                anyhow::bail!("execution state awaits durable persistence")
            }
            Some(record) => ExecutionReply::Inspected {
                receipt: record.phase.receipt().cloned(),
                state: record.phase.state(),
                output_end: record.output_end.clone(),
            },
            None => ExecutionReply::Absent,
        })
    }
    pub(super) fn persist_tombstone(&self, tombstone: ExecutionTombstone) -> anyhow::Result<()> {
        let id = tombstone.execution_id.clone();
        if let Some(previous) = self.tombstones.lock().map_err(poison)?.get(&id) {
            anyhow::ensure!(*previous == tombstone, "immutable tombstone conflict");
        }
        // Tombstone is durable before removing hot state; it wins even over leftover journals.
        self.store
            .write(&format!("{}.tombstone", id.as_str()), &tombstone)?;
        self.tombstones
            .lock()
            .map_err(poison)?
            .insert(id.clone(), tombstone);
        self.release_private(&id)?;
        self.records.lock().map_err(poison)?.remove(&id);
        self.preparations.retire(&id)?;
        self.processes.lock().map_err(poison)?.remove(&id);
        let removal = self.store.remove_execution(&id);
        // Observers wake only after cleanup was attempted, including a retryable IO failure.
        self.changed(id)?;
        removal?;
        Ok(())
    }
    pub(super) async fn retire_execution(
        &self,
        id: ExecutionId,
        lifecycle: CapabilityToken,
    ) -> anyhow::Result<ExecutionReply> {
        let gate = self.id_gate(&id)?;
        let _id = gate.lock().await;
        if let Some(tombstone) = self.tombstones.lock().map_err(poison)?.get(&id) {
            self.store.remove_execution(&id)?;
            return Ok(ExecutionReply::Retired(tombstone.clone()));
        }
        let first = self.record(&id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        let _fence = gate.lock().await;
        let record = self.record(&id)?;
        anyhow::ensure!(
            record.request.control_tokens.lifecycle.matches(&lifecycle),
            "invalid lifecycle token"
        );
        if record.output_end == OutputEnd::Open {
            return Ok(ExecutionReply::Busy(BusyReason::OutputOpen));
        }
        anyhow::ensure!(record.durably_settled(), "execution not durably settled");
        self.outputs.lock().map_err(poison)?.remove(&id);
        self.store.remove(&format!("{}.output", id.as_str()))?;
        let tombstone = ExecutionTombstone {
            execution_id: id,
            ticket_id: record.request.identity.ticket_id.clone(),
            registration: Some(record.request.identity.source_registration.clone()),
            directory: match &record.request.source {
                PreparedSource::Private(t) => match &t.registration {
                    ExecutionRegistration::ExternalDir { directory_id, .. } => {
                        Some(directory_id.clone())
                    }
                    _ => None,
                },
                _ => None,
            },
            slot: record
                .phase
                .slot_version()
                .map(|v| (record.request.identity.process_alias.clone(), v)),
            outcome: TombstoneOutcome::Retired,
        };
        self.persist_tombstone(tombstone.clone())?;
        Ok(ExecutionReply::Retired(tombstone))
    }
}
