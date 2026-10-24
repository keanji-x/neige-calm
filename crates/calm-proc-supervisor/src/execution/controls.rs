use super::*;
use calm_session::control::IoMode;

impl Executor {
    pub(super) async fn revoke(&self, request: ConsumeExecution) -> anyhow::Result<ExecutionReply> {
        let id_gate = self.id_gate(&request.execution_id)?;
        let _id = id_gate.lock().await;
        if let Some(t) = self
            .tombstones
            .lock()
            .map_err(poison)?
            .get(&request.execution_id)
        {
            anyhow::ensure!(
                t.ticket_id == request.ticket_id,
                "tombstone ticket mismatch"
            );
            return Self::replay_retired(&self.store, t);
        }
        if !self
            .records
            .lock()
            .map_err(poison)?
            .contains_key(&request.execution_id)
        {
            self.preparations.reserve_revocation(&request)?;
            if let Some(tombstone) = self
                .tombstones
                .lock()
                .map_err(poison)?
                .values()
                .find(|t| t.ticket_id == request.ticket_id)
            {
                Self::replay_retired(&self.store, tombstone)?;
                anyhow::bail!("ticket belongs to a tombstone");
            }
            let tombstone = ExecutionTombstone {
                execution_id: request.execution_id,
                ticket_id: request.ticket_id,
                registration: None,
                directory: None,
                slot: None,
                outcome: TombstoneOutcome::Revoked,
            };
            return self.persist_tombstone(tombstone);
        }
        let first = self.record(&request.execution_id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        let _fence = gate.lock().await;
        let mut record = self.record(&request.execution_id)?;
        Self::check_ticket(&record, &request)?;
        record.interactive_revoked = true;
        record.phase = match record.phase {
            Phase::Prepared => Phase::Revoked,
            Phase::Reserved { slot_version } => Phase::NoChild {
                slot_version: Some(slot_version),
                reason: "revoked before fork barrier".into(),
            },
            phase => phase,
        };
        // IO failure seals interaction in this boot but cannot acknowledge durable revocation.
        // Every retry fsyncs again, so a previous in-memory latch cannot manufacture an ACK.
        if let Err(error) = self.persist(record.clone()) {
            self.latch(record)?;
            return Err(error);
        }
        Ok(ExecutionReply::RevocationAcknowledged {
            execution_id: request.execution_id,
            ticket_id: request.ticket_id,
            receipt: record.phase.receipt().cloned(),
            state: record.phase.state(),
        })
    }

    pub(super) async fn control(
        &self,
        control: ExecutionControl,
    ) -> anyhow::Result<ExecutionReply> {
        let id = &control.capability.identity.execution_id;
        let first = self.record(id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        loop {
            let _fence = gate.lock().await;
            let record = self.record(id)?;
            let receipt = record
                .phase
                .receipt()
                .ok_or_else(|| anyhow::anyhow!("execution has no process receipt"))?;
            anyhow::ensure!(
                receipt.identity == control.capability.identity
                    && receipt.slot_version == control.capability.slot_version,
                "stale execution capability"
            );
            let tokens = &record.request.control_tokens;
            let token = match control.capability.purpose {
                ExecutionControlPurpose::Observe => &tokens.observe,
                ExecutionControlPurpose::Interactive => &tokens.interactive,
                ExecutionControlPurpose::Lifecycle => &tokens.lifecycle,
            };
            anyhow::ensure!(
                token.matches(&control.capability.token),
                "invalid execution control capability"
            );
            let required = match &control.action {
                ProcessAction::ReservedCleanup(never) => match *never {},
                ProcessAction::Observe { .. } => ExecutionControlPurpose::Observe,
                ProcessAction::WriteStdin { .. }
                | ProcessAction::CloseStdin
                | ProcessAction::Resize { .. } => ExecutionControlPurpose::Interactive,
                ProcessAction::Signal(_) | ProcessAction::Stop => {
                    ExecutionControlPurpose::Lifecycle
                }
            };
            anyhow::ensure!(
                control.capability.purpose == required,
                "wrong execution control purpose"
            );
            if required == ExecutionControlPurpose::Interactive {
                anyhow::ensure!(
                    !record.interactive_revoked,
                    "exact execution interaction explicitly revoked"
                );
                if matches!(control.action, ProcessAction::CloseStdin) {
                    anyhow::ensure!(
                        matches!(record.request.specification.io_mode, IoMode::Pipe),
                        "close stdin requires Pipe mode"
                    );
                    // Settlement released the private source. Authenticate the retained receipt above,
                    // but do not resolve that source again for an exact terminal retry.
                    if matches!(
                        record.phase.state(),
                        ExecutionState::Drained { .. } | ExecutionState::OriginalGroupEmpty { .. }
                    ) && record.durably_settled()
                    {
                        return Ok(ExecutionReply::ActionComplete);
                    }
                }
                // Native handles may survive a loss of drain proof. Their presence is
                // not authority to interact with an Unknown execution.
                anyhow::ensure!(
                    matches!(
                        record.phase.state(),
                        ExecutionState::Spawned | ExecutionState::Exited { .. }
                    ),
                    "execution state does not permit interaction"
                );
                self.resolve(&record.request.identity, true)?;
            }
            let process = self.processes.lock().map_err(poison)?.get(id).cloned();
            match &control.action {
                ProcessAction::Observe { from_cursor } => {
                    let log = self.outputs.lock().map_err(poison)?.get(id).cloned();
                    let (cursor, output) = match log {
                        Some(log) => log.snapshot(*from_cursor)?,
                        None => (0, Vec::new()),
                    };
                    return Ok(ExecutionReply::Observation {
                        receipt: Some(receipt.clone()),
                        state: record.phase.state(),
                        cursor,
                        output,
                        output_end: record.output_end.clone(),
                    });
                }
                ProcessAction::Stop => {
                    if !record.durably_settled() {
                        self.stop_execution(id)?;
                    }
                    return Ok(ExecutionReply::ActionComplete);
                }
                action => {
                    anyhow::ensure!(
                        !record.phase.state().permits_handoff(),
                        "execution already terminal"
                    );
                    let process = process.ok_or_else(|| {
                        anyhow::anyhow!("original native execution unavailable; recovery required")
                    })?;
                    match action {
                        ProcessAction::WriteStdin { bytes } => {
                            #[cfg(target_os = "linux")]
                            if matches!(record.request.specification.io_mode, IoMode::Pipe) {
                                // Register before checking: Started/EOF/Stop may arrive as soon as
                                // the scope gate is released. Event application needs that same gate.
                                let changed = process.startup_changed.notified();
                                tokio::pin!(changed);
                                changed.as_mut().enable();
                                if !process.started.load(std::sync::atomic::Ordering::SeqCst) {
                                    anyhow::ensure!(
                                        !process
                                            .reports_ended
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                            && process
                                                .stop_deadline
                                                .lock()
                                                .map_err(poison)?
                                                .is_none(),
                                        "execution startup ended before stdin handoff"
                                    );
                                    drop(_fence);
                                    changed.await;
                                    // Re-enter the complete capability, state and source validation.
                                    continue;
                                }
                            }
                            return Ok(ExecutionReply::StdinProgress {
                                accepted_bytes: process.write(bytes)?,
                            });
                        }
                        ProcessAction::CloseStdin => process.close_stdin()?,
                        ProcessAction::Resize {
                            cols,
                            rows,
                            pixel_w,
                            pixel_h,
                        } => process.resize(*cols, *rows, *pixel_w, *pixel_h)?,
                        ProcessAction::Signal(signal) => process.signal(*signal)?,
                        ProcessAction::ReservedCleanup(never) => match *never {},
                        ProcessAction::Observe { .. } | ProcessAction::Stop => {
                            unreachable!("handled above")
                        }
                    }
                    return Ok(ExecutionReply::ActionComplete);
                }
            }
        }
    }
}
