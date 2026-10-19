use super::*;

#[derive(Default)]
struct State {
    requests: HashMap<ExecutionId, PrepareExecution>,
    tickets: HashMap<LaunchTicketId, ExecutionId>,
    attachments: HashMap<ObjectRegistrationId, (AttachRegistration, Option<ExecutionId>)>,
}

/// One in-memory index of the journal's immutable identity claims, including pending fsyncs.
#[derive(Default)]
pub(super) struct PreparationReservations(StdMutex<State>);

impl PreparationReservations {
    pub(super) fn from_records(records: &HashMap<ExecutionId, Journal>) -> anyhow::Result<Self> {
        let reservations = Self::default();
        for record in records.values() {
            reservations.reserve(&record.request)?;
        }
        Ok(reservations)
    }

    pub(super) fn retire(&self, id: &ExecutionId) -> anyhow::Result<()> {
        let mut state = self.0.lock().map_err(poison)?;
        state.requests.remove(id);
        state
            .attachments
            .retain(|_, (_, owner)| owner.as_ref() != Some(id));
        Ok(())
    }

    pub(super) fn reserve_revocation(&self, request: &ConsumeExecution) -> anyhow::Result<()> {
        let mut state = self.0.lock().map_err(poison)?;
        anyhow::ensure!(
            state
                .tickets
                .get(&request.ticket_id)
                .is_none_or(|id| *id == request.execution_id),
            "ticket already belongs to another execution"
        );
        if let Some(prepared) = state.requests.get(&request.execution_id) {
            anyhow::ensure!(
                prepared.identity.ticket_id == request.ticket_id
                    && prepared.ticket.matches(&request.ticket),
                "revocation conflicts with reserved preparation"
            );
        }
        state
            .tickets
            .insert(request.ticket_id.clone(), request.execution_id.clone());
        Ok(())
    }

    fn reserve_transfer(
        state: &mut State,
        transfer: &AttachRegistration,
        owner: Option<&ExecutionId>,
    ) -> anyhow::Result<()> {
        for (known, known_owner) in state.attachments.values() {
            if known.registration_id == transfer.registration_id || known.nonce == transfer.nonce {
                anyhow::ensure!(
                    known == transfer && known_owner.as_ref() == owner,
                    "transfer identity already belongs to another attachment/execution"
                );
            }
            if owner.is_some() || known_owner.is_some() {
                anyhow::ensure!(
                    registration_resource(&known.registration)
                        != registration_resource(&transfer.registration)
                        || (known == transfer && known_owner.as_ref() == owner),
                    "private directory already reserved by another attachment/execution"
                );
            }
        }
        state.attachments.insert(
            transfer.registration_id.clone(),
            (transfer.clone(), owner.cloned()),
        );
        Ok(())
    }

    fn reserve(&self, request: &PrepareExecution) -> anyhow::Result<()> {
        let mut state = self.0.lock().map_err(poison)?;
        let identity = &request.identity;
        if let Some(existing) = state.requests.get(&identity.execution_id) {
            anyhow::ensure!(
                existing == request,
                "execution identity reserved with different immutable input"
            );
            return Ok(());
        }
        anyhow::ensure!(
            !state.tickets.contains_key(&identity.ticket_id),
            "ticket already belongs to another execution"
        );
        if let PreparedSource::Private(transfer) = &request.source {
            Self::reserve_transfer(&mut state, transfer, Some(&identity.execution_id))?;
        }
        state
            .tickets
            .insert(identity.ticket_id.clone(), identity.execution_id.clone());
        state
            .requests
            .insert(identity.execution_id.clone(), request.clone());
        Ok(())
    }
}

impl Executor {
    pub(super) async fn prepare(
        &self,
        request: PrepareExecution,
    ) -> anyhow::Result<ExecutionReply> {
        self.prepare_objects(request, Vec::new()).await
    }

    pub(super) async fn prepare_objects(
        &self,
        request: PrepareExecution,
        descriptors: Vec<std::os::fd::OwnedFd>,
    ) -> anyhow::Result<ExecutionReply> {
        let id_gate = self.id_gate(&request.identity.execution_id)?;
        let _id = id_gate.lock().await;
        {
            let tombstones = self.tombstones.lock().map_err(poison)?;
            if let Some(tombstone) = tombstones.get(&request.identity.execution_id) {
                Self::replay_retired(&self.store, tombstone)?;
                anyhow::bail!("execution already retired or revoked");
            }
            if let Some(tombstone) = tombstones
                .values()
                .find(|t| t.ticket_id == request.identity.ticket_id)
            {
                Self::replay_retired(&self.store, tombstone)?;
                anyhow::bail!("ticket belongs to a tombstone");
            }
        }
        anyhow::ensure!(
            !self
                .store
                .contains_tombstone(&request.identity.execution_id)?,
            "execution has a tombstone persistence claim"
        );
        let gate = self.gate(scope_resource(&request.identity.scope))?;
        let _fence = gate.lock().await;
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::SeqCst),
            "executor admission closed"
        );
        let existing = self
            .records
            .lock()
            .map_err(poison)?
            .get(&request.identity.execution_id)
            .cloned();
        if let Some(existing) = &existing {
            anyhow::ensure!(
                existing.request == request,
                "execution identity reused with different immutable input"
            );
            // Terminal/historical exact retries return their original facts without recreating
            // source authority. Registered still-Prepared retries satisfy today's qualification;
            // private exact retries retain their original description even after path replacement.
            if matches!(request.source, PreparedSource::Private(_))
                || !matches!(existing.phase, Phase::Prepared | Phase::Reserved { .. })
            {
                return self.replay_reply(existing);
            }
        }
        anyhow::ensure!(
            request.identity.executor_boot == self.boot,
            "wrong executor boot identity"
        );
        anyhow::ensure!(
            !request.identity.process_alias.is_empty()
                && request.identity.process_alias.len() <= 512,
            "invalid process alias"
        );
        anyhow::ensure!(
            request.specification.program.is_absolute(),
            "execution program must be explicit absolute path"
        );
        anyhow::ensure!(
            request
                .specification
                .environment
                .iter()
                .all(|(key, value)| !key.is_empty()
                    && !key.contains(['=', '\0'])
                    && !value.contains('\0')),
            "invalid explicit execution environment"
        );
        let staged = match &request.source {
            PreparedSource::Private(transfer) => {
                Some(self.stage_private(&request, transfer, descriptors)?)
            }
            PreparedSource::Registered => {
                anyhow::bail!("shared registration preparation is no longer supported");
            }
        };
        if let Some(existing) = existing {
            return self.replay_reply(&existing);
        }
        // Immutable scope equality makes exact retries share the same resource gate.
        // The global reservation is released before journal IO, and survives failed persistence.
        self.preparations.reserve(&request)?;
        self.persist(Journal {
            version: journal::JOURNAL_VERSION,
            host_boot_id: self.host_boot_id.clone(),
            output_end: OutputEnd::Open,
            request,
            phase: Phase::Prepared,
            interactive_revoked: false,
            pending_persistence: false,
        })?;
        if let Some(root) = staged {
            self.publish_private(root);
        }
        Ok(ExecutionReply::Prepared)
    }
}
