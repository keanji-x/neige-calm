use super::*;

#[derive(Default)]
struct State {
    requests: HashMap<ExecutionId, PrepareExecution>,
    tickets: HashMap<LaunchTicketId, ExecutionId>,
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
        let gate = self.gate(scope_resource(&request.identity.scope))?;
        let _fence = gate.lock().await;
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::SeqCst),
            "executor admission closed"
        );
        if let Ok(existing) = self.record(&request.identity.execution_id) {
            anyhow::ensure!(
                existing.request == request,
                "execution identity reused with different immutable input"
            );
            return Ok(reply(&existing));
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
        self.resolve(&request.identity, true)?;
        // Immutable scope equality makes exact retries share the same resource gate.
        // The global reservation is released before journal IO, and survives failed persistence.
        self.preparations.reserve(&request)?;
        self.persist(Journal {
            version: 2,
            request,
            phase: Phase::Prepared,
        })?;
        Ok(ExecutionReply::Prepared)
    }
}
