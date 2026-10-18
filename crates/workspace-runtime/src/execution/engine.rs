use super::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, watch};

struct Slot {
    dirty: bool,
    notify: Arc<Notify>,
}
struct Inner<S, X> {
    store: S,
    executor: X,
    slots: Mutex<HashMap<ExecutionId, Slot>>,
    errors: Mutex<HashMap<ExecutionId, DriveError>>,
    changed: watch::Sender<u64>,
}
pub struct Engine<S, X>(Arc<Inner<S, X>>);
impl<S, X> Clone for Engine<S, X> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
#[derive(Default, Debug)]
pub struct RecoverReport {
    pub scheduled: usize,
    pub error: Option<StoreError>,
}
impl<S: Store + 'static, X: Executor + 'static> Engine<S, X> {
    pub fn new(store: S, executor: X) -> Self {
        Self(Arc::new(Inner {
            store,
            executor,
            slots: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            changed: watch::channel(0).0,
        }))
    }
    pub fn available(&self) -> bool {
        self.0.executor.available()
    }
    pub async fn admit(
        &self,
        tx: &mut S::Tx<'_>,
        intent: ExecutionIntent,
    ) -> Result<PendingDriveTicket, StoreError> {
        self.0.store.insert(tx, &intent).await?;
        Ok(PendingDriveTicket(intent.metadata.id))
    }
    pub async fn request(
        &self,
        tx: &mut S::Tx<'_>,
        id: &ExecutionId,
        desired: Desired,
    ) -> Result<PendingDriveTicket, StoreError> {
        self.0.store.request(tx, id, desired).await?;
        Ok(PendingDriveTicket(id.clone()))
    }
    pub fn drive(&self, ticket: DriveTicket) {
        let id = ticket.0;
        let mut slots = self.0.slots.lock().unwrap();
        if let Some(slot) = slots.get_mut(&id) {
            slot.dirty = true;
            slot.notify.notify_one();
            return;
        }
        let notify = Arc::new(Notify::new());
        slots.insert(
            id.clone(),
            Slot {
                dirty: false,
                notify: notify.clone(),
            },
        );
        let engine = self.clone();
        tokio::spawn(async move {
            engine.run(id, notify).await;
        });
    }
    pub async fn recover(&self) -> RecoverReport {
        match self.0.store.pending().await {
            Ok(ids) => {
                let scheduled = ids.len();
                for id in ids {
                    self.drive(DriveTicket::committed(id));
                }
                RecoverReport {
                    scheduled,
                    error: None,
                }
            }
            Err(error) => RecoverReport {
                scheduled: 0,
                error: Some(error),
            },
        }
    }
    #[cfg(test)]
    pub(super) fn driving(&self, id: &ExecutionId) -> bool {
        self.0.slots.lock().unwrap().contains_key(id)
    }
    pub fn error(&self, id: &ExecutionId) -> Option<DriveError> {
        self.0.errors.lock().unwrap().get(id).cloned()
    }
    fn changed(&self) {
        self.0.changed.send_modify(|g| *g += 1);
    }
    async fn run(self, id: ExecutionId, notify: Arc<Notify>) {
        loop {
            self.0.slots.lock().unwrap().get_mut(&id).unwrap().dirty = false;
            self.0.errors.lock().unwrap().remove(&id);
            match self.cycle(&id, &notify).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    self.0.errors.lock().unwrap().insert(id.clone(), error);
                }
            }
            // Park handshake: the same lock covers drive's dirty write and removal.
            let mut slots = self.0.slots.lock().unwrap();
            if slots.get(&id).unwrap().dirty {
                continue;
            }
            slots.remove(&id);
            drop(slots);
            self.changed();
            return;
        }
    }
    async fn wait(&self, generation: Generation, notify: &Notify) -> Result<bool, DriveError> {
        tokio::select! {
            result = generation.changed() => result?,
            _ = notify.notified() => {},
        }
        Ok(true)
    }
    async fn cycle(&self, id: &ExecutionId, notify: &Notify) -> Result<bool, DriveError> {
        // Finished rows need no executor connection (including Watch).
        if !matches!(self.0.store.load(id).await?, Some(Stored::Active { .. })) {
            return Ok(false);
        }
        // Snapshot before reloading: a concurrent request must not lose its wake.
        let generation = self.0.executor.generation().await?;
        let Some(Stored::Active {
            intent,
            secrets,
            desired,
            settlement,
        }) = self.0.store.load(id).await?
        else {
            return Ok(false);
        };
        let intent = ExecutionIntent {
            metadata: intent,
            secrets,
        };
        let fact = match self.0.executor.inspect(id).await {
            Ok(fact) => fact,
            Err(ExecError::Transport(_)) => return self.wait(generation, notify).await,
            Err(error) => return Err(error.into()),
        };
        let step = next_step(desired, &fact);
        if matches!(
            fact,
            Fact::Journal {
                state: ExecutionState::Spawned,
                ..
            }
        ) {
            self.0.store.spawned(id).await?;
        }
        let result = match step {
            Step::Prepare => self.0.executor.prepare(&intent).await,
            Step::Launch => self.0.executor.launch(&intent.consume()).await,
            Step::Revoke => self.0.executor.revoke(&intent.consume()).await,
            Step::Stop => {
                // Forking has no receipt yet. The executor's in-flight launch owns the
                // next transition; wait for its receipt rather than invent a capability.
                if matches!(fact, Fact::Journal { receipt: None, .. }) {
                    return self.wait(generation, notify).await;
                }
                let capability = capability(
                    &fact,
                    &intent.secrets.tokens.lifecycle,
                    ExecutionControlPurpose::Lifecycle,
                )?;
                self.0
                    .executor
                    .control(ExecutionControl {
                        capability,
                        action: ProcessAction::Stop,
                    })
                    .await
                    .map(|_| fact.clone())
            }
            Step::Wait => return self.wait(generation, notify).await,
            Step::Settle | Step::Finish => {
                if settlement.is_none() {
                    self.0
                        .store
                        .settle(id, &Settlement::from_fact(&fact))
                        .await?;
                    self.changed();
                }
                if step == Step::Finish {
                    self.0.store.finish(id).await?;
                    self.changed();
                    return Ok(false);
                }
                if desired != Desired::Retire {
                    return Ok(false);
                }
                self.0
                    .executor
                    .retire(id, &intent.secrets.tokens.lifecycle)
                    .await
            }
        };
        match result {
            Ok(_) if step == Step::Stop => self.wait(generation, notify).await,
            Ok(_) => Ok(true),
            Err(ExecError::Busy(_) | ExecError::Transport(_)) => {
                self.wait(generation, notify).await
            }
            Err(ExecError::Refused(_)) if matches!(step, Step::Prepare | Step::Launch) => {
                match self.0.executor.revoke(&intent.consume()).await {
                    Ok(_) => Ok(true),
                    Err(ExecError::Transport(_) | ExecError::Busy(_)) => {
                        self.wait(generation, notify).await
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => Err(error.into()),
        }
    }
    pub async fn wait_settled(&self, id: &ExecutionId) -> Result<(), DriveError> {
        let mut changes = self.0.changed.subscribe();
        let wait = async {
            loop {
                let row = self.0.store.load(id).await?.ok_or(DriveError::Missing)?;
                if row.is_settled() {
                    return Ok(());
                }
                if let Some(error) = self.error(id) {
                    return Err(error);
                }
                changes.changed().await.map_err(|_| DriveError::Missing)?;
            }
        };
        tokio::time::timeout(QUIESCE_DEADLINE, wait)
            .await
            .map_err(|_| DriveError::Timeout)?
    }
    /// Subscribe before reading output, so concurrent appends cannot be missed.
    pub async fn output_generation(&self) -> Result<Generation, DriveError> {
        Ok(self.0.executor.generation().await?)
    }
    pub async fn output(
        &self,
        id: &ExecutionId,
        from_cursor: Option<u64>,
    ) -> Result<ExecutionReply, DriveError> {
        self.control(id, ProcessAction::Observe { from_cursor })
            .await
    }
    pub async fn interact(
        &self,
        id: &ExecutionId,
        action: ProcessAction,
    ) -> Result<ExecutionReply, DriveError> {
        if !matches!(
            action,
            ProcessAction::WriteStdin { .. }
                | ProcessAction::Resize { .. }
                | ProcessAction::Signal(_)
        ) {
            return Err(ExecError::Refused("not an interactive action".into()).into());
        }
        self.control(id, action).await
    }
    async fn control(
        &self,
        id: &ExecutionId,
        action: ProcessAction,
    ) -> Result<ExecutionReply, DriveError> {
        let row = self.0.store.load(id).await?.ok_or(DriveError::Missing)?;
        if !matches!(action, ProcessAction::Observe { .. })
            && matches!(row, Stored::Finished { .. })
        {
            return Err(ExecError::Refused("execution has no active capabilities".into()).into());
        }
        let fact = self.0.executor.inspect(id).await?;
        if matches!(action, ProcessAction::Observe { .. }) {
            match &fact {
                Fact::Retired(tombstone) => return Ok(ExecutionReply::Retired(tombstone.clone())),
                Fact::Journal {
                    state,
                    receipt: None,
                    output: OutputEnd::Complete { final_cursor: 0 },
                } => {
                    // Never-forked executions have no output capability or bytes to read.
                    return Ok(ExecutionReply::Observation {
                        receipt: None,
                        state: state.clone(),
                        cursor: 0,
                        output: Vec::new(),
                        output_end: OutputEnd::Complete { final_cursor: 0 },
                    });
                }
                _ => {}
            }
        }
        let Stored::Active { secrets, .. } = row else {
            return Err(ExecError::Refused("execution has no active capabilities".into()).into());
        };
        let (token, purpose) = if matches!(action, ProcessAction::Observe { .. }) {
            (&secrets.tokens.observe, ExecutionControlPurpose::Observe)
        } else if matches!(action, ProcessAction::Signal(_)) {
            (
                &secrets.tokens.lifecycle,
                ExecutionControlPurpose::Lifecycle,
            )
        } else {
            (
                &secrets.tokens.interactive,
                ExecutionControlPurpose::Interactive,
            )
        };
        Ok(self
            .0
            .executor
            .control(ExecutionControl {
                capability: capability(&fact, token, purpose)?,
                action,
            })
            .await?)
    }
}
fn capability(
    fact: &Fact,
    token: &CapabilityToken,
    purpose: ExecutionControlPurpose,
) -> Result<ExecutionCapability, ExecError> {
    let Fact::Journal {
        receipt: Some(receipt),
        ..
    } = fact
    else {
        return Err(ExecError::Refused("execution has no receipt".into()));
    };
    Ok(ExecutionCapability {
        identity: receipt.identity.clone(),
        slot_version: receipt.slot_version,
        token: token.clone(),
        purpose,
    })
}
