//! Durable executor mechanisms. Authorization and scheduling decisions remain in kernel.

mod attachment;
mod directory;
mod directory_probe;
mod directory_relation;
mod journal;
mod launch;
#[cfg(test)]
mod monitor_tests;
mod native_child;
mod preparation;
mod process;
mod retirement;
mod scope_helper;
mod shutdown;
mod validation;
use validation::*;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use calm_session::control::execution::*;
use directory::RegisteredRoot;
use journal::{Journal, Phase, Store};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ExecutorConfig {
    pub journal_directory: PathBuf,
    pub helper_program: PathBuf,
    pub kernel_client_secret: CapabilityToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ResourceKey {
    Worktree(String),
    External(String),
}

#[derive(Clone)]
struct AliasSlot {
    execution: Option<ExecutionId>,
    version: u64,
}

/// One instance owns this journal exclusively. Historical receipts never name the alias's replacement.
pub struct Executor {
    closing: std::sync::atomic::AtomicBool,
    store: Store,
    config: ExecutorConfig,
    boot: ExecutorBootId,
    helper_image: std::fs::File,
    mechanism: ScopeMechanismIdentity,
    legacy_persistence: StdMutex<()>,
    gates: StdMutex<HashMap<ResourceKey, Arc<Mutex<()>>>>,
    registrations: StdMutex<HashMap<String, ObjectRegistrationReceipt>>,
    registration_facts: StdMutex<HashMap<String, ExecutionRegistration>>,
    transfers: StdMutex<HashMap<TransferNonce, ObjectRegistrationReceipt>>,
    live_objects: StdMutex<HashMap<ObjectRegistrationId, Arc<RegisteredRoot>>>,
    retired: StdMutex<HashMap<ObjectRegistrationId, ObjectRegistrationReceipt>>,
    preparations: preparation::PreparationReservations,
    records: StdMutex<HashMap<ExecutionId, Journal>>,
    aliases: StdMutex<HashMap<String, AliasSlot>>,
    processes: StdMutex<HashMap<ExecutionId, Arc<process::Process>>>,
}

impl Executor {
    pub fn open(config: ExecutorConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.helper_program.is_absolute(),
            "helper program must be absolute"
        );
        let store = Store::open(config.journal_directory.clone())?;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let helper_image = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&config.helper_program)?;
        let image = helper_image.metadata()?;
        anyhow::ensure!(
            image.is_file() && image.mode() & 0o111 != 0,
            "helper executable not usable"
        );
        let mechanism = ScopeMechanismIdentity {
            executable_device: image.dev(),
            executable_inode: image.ino(),
            protocol_version: 1,
        };
        let mut records = HashMap::new();
        let mut aliases: HashMap<String, AliasSlot> = HashMap::new();
        for mut record in store.load()? {
            anyhow::ensure!(record.version == 3, "unsupported execution journal version");
            let identity = &record.request.identity;
            if let Some(receipt) = record.phase.receipt() {
                anyhow::ensure!(
                    receipt.identity == *identity,
                    "receipt identity does not match its journal"
                );
            }
            if let Phase::Drained { receipt, proof, .. } = &record.phase {
                anyhow::ensure!(
                    proof.helper_process == receipt.process
                        && proof.mechanism == receipt.scope_mechanism,
                    "drain proof does not bind original scope receipt"
                );
            }
            record.phase = match &record.phase {
                Phase::Prepared => { record.interactive_revoked = true; Phase::Revoked },
                Phase::Reserved { slot_version } => Phase::NoChild { slot_version: *slot_version, reason: "executor restarted before fork barrier".into() },
                phase if !phase.state().permits_handoff() => Phase::Unknown { slot_version: phase.slot_version().ok_or_else(|| anyhow::anyhow!("missing active alias version"))?, receipt: phase.receipt().cloned(), reason: "executor restarted without original native handles; reconciliation required".into() },
                phase => phase.clone(),
            };
            store.save(&record)?;
            if let Some(version) = record.phase.slot_version() {
                let slot = aliases
                    .entry(identity.process_alias.clone())
                    .or_insert(AliasSlot {
                        execution: Some(identity.execution_id.clone()),
                        version,
                    });
                anyhow::ensure!(
                    slot.version != version
                        || slot.execution.as_ref() == Some(&identity.execution_id),
                    "conflicting durable alias slots"
                );
                if version > slot.version {
                    *slot = AliasSlot {
                        execution: Some(identity.execution_id.clone()),
                        version,
                    };
                }
            }
            anyhow::ensure!(
                records
                    .insert(identity.execution_id.clone(), record)
                    .is_none(),
                "duplicate execution journal"
            );
        }
        let legacy_path = store.path().join("legacy.aliases");
        if legacy_path.exists() {
            let legacy: Vec<String> = serde_json::from_reader(std::fs::File::open(legacy_path)?)?;
            for alias in legacy {
                anyhow::ensure!(
                    !aliases.contains_key(&alias),
                    "legacy/typed alias ownership conflict"
                );
                aliases.insert(
                    alias,
                    AliasSlot {
                        execution: None,
                        version: 0,
                    },
                );
            }
        }
        let mut registration_facts = HashMap::new();
        let mut transfers = HashMap::new();
        // Serialized receipts retain audit/version facts only. No descriptor survives this boot.
        for receipt in store.registrations()? {
            anyhow::ensure!(
                receipt.contract == EXECUTION_ENVELOPE_VERSION
                    && receipt.id == receipt.transfer.registration_id,
                "unsupported persisted attachment contract or provenance"
            );
            let registration = &receipt.transfer.registration;
            let key = registration_key(registration);
            if let Some(previous) = registration_facts.get(&key) {
                if registration_successor(previous, registration) {
                    registration_facts.insert(key, registration.clone());
                } else {
                    anyhow::ensure!(
                        registration_successor(registration, previous),
                        "conflicting persisted registrations"
                    );
                }
            } else {
                registration_facts.insert(key, registration.clone());
            }
            anyhow::ensure!(
                transfers
                    .insert(receipt.transfer.nonce.clone(), receipt)
                    .is_none(),
                "duplicate persisted attachment nonce"
            );
        }
        let mut retired = HashMap::new();
        for receipt in store.retirements()? {
            anyhow::ensure!(
                transfers.get(&receipt.transfer.nonce) == Some(&receipt),
                "retired lease does not match immutable registration history"
            );
            anyhow::ensure!(
                retired.insert(receipt.id.clone(), receipt).is_none(),
                "duplicate retired registration identity"
            );
        }
        let executor = Arc::new(Self {
            closing: std::sync::atomic::AtomicBool::new(false),
            store,
            config,
            boot: ExecutorBootId::fresh()?,
            helper_image,
            mechanism,
            legacy_persistence: Default::default(),
            gates: Default::default(),
            registrations: Default::default(),
            registration_facts: StdMutex::new(registration_facts),
            transfers: StdMutex::new(transfers),
            live_objects: Default::default(),
            retired: StdMutex::new(retired),
            preparations: preparation::PreparationReservations::from_records(&records)?,
            records: StdMutex::new(records),
            aliases: StdMutex::new(aliases),
            processes: Default::default(),
        });
        executor.store.write("executor.boot", &executor.boot)?;
        Ok(executor)
    }

    pub fn boot(&self) -> &ExecutorBootId {
        &self.boot
    }
    pub fn authenticates(&self, secret: &CapabilityToken) -> bool {
        self.config.kernel_client_secret.matches(secret)
    }

    pub async fn dispatch(
        self: &Arc<Self>,
        secret: &CapabilityToken,
        request: ExecutionRequest,
    ) -> ExecutionReply {
        if !self.authenticates(secret) {
            return refused("unregistered kernel client");
        }
        match self.dispatch_authorized(request).await {
            Ok(reply) => reply,
            Err(error) => qualification_or_refusal(&error),
        }
    }

    async fn dispatch_authorized(
        self: &Arc<Self>,
        request: ExecutionRequest,
    ) -> anyhow::Result<ExecutionReply> {
        match request {
            ExecutionRequest::Handshake(hello) => Ok(ExecutionReply::Handshake {
                envelope_version: EXECUTION_ENVELOPE_VERSION,
                executor_boot: self.boot.clone(),
                mode: hello.mode,
            }),
            ExecutionRequest::Attach(_)
            | ExecutionRequest::Export(_)
            | ExecutionRequest::ExportAck(_) => {
                anyhow::bail!("object attachment requires the controlled ancillary transport")
            }
            ExecutionRequest::RetireRegistration(receipt) => {
                self.retire_registration(receipt).await
            }
            ExecutionRequest::Prepare(request) => self.prepare(request).await,
            ExecutionRequest::Launch(request) => self.launch(request).await,
            ExecutionRequest::Revoke(request) => self.revoke(request).await,
            ExecutionRequest::Control(control) => self.control(control).await,
            ExecutionRequest::ObserveDirectories(request) => {
                self.observe_directories(*request).await
            }
        }
    }

    fn gate(&self, key: ResourceKey) -> anyhow::Result<Arc<Mutex<()>>> {
        Ok(self
            .gates
            .lock()
            .map_err(poison)?
            .entry(key)
            .or_default()
            .clone())
    }
    fn record(&self, id: &ExecutionId) -> anyhow::Result<Journal> {
        self.records
            .lock()
            .map_err(poison)?
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown execution identity"))
    }
    fn persist(&self, mut record: Journal) -> anyhow::Result<()> {
        self.store.save(&record)?;
        record.pending_persistence = false;
        self.records
            .lock()
            .map_err(poison)?
            .insert(record.request.identity.execution_id.clone(), record);
        Ok(())
    }

    fn replay_reply(&self, record: &Journal) -> anyhow::Result<ExecutionReply> {
        // Unknown remains an uncertainty report; a new positive settlement needs its barrier.
        if record.pending_persistence && record.phase.state().permits_handoff() {
            self.persist(record.clone())?;
        }
        Ok(reply(record))
    }

    fn latch(&self, mut record: Journal) -> anyhow::Result<Journal> {
        record.pending_persistence = true;
        self.records
            .lock()
            .map_err(poison)?
            .insert(record.request.identity.execution_id.clone(), record.clone());
        Ok(record)
    }

    fn resolve(
        &self,
        identity: &ExecutionIdentity,
        require_admission: bool,
    ) -> anyhow::Result<Arc<RegisteredRoot>> {
        // Source/authorization belong to the referenced immutable receipt, never the latest grant.
        let root = self
            .live_objects
            .lock()
            .map_err(poison)?
            .get(&identity.source_registration)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("source registration is Unwitnessed for this executor")
            })?;
        anyhow::ensure!(
            root.receipt.transfer.incarnation == identity.source_incarnation
                && root.receipt.transfer.executor_boot == identity.executor_boot,
            "unwitnessed or stale source registration receipt"
        );
        anyhow::ensure!(
            registration_matches(&root.registration, identity, require_admission),
            "stale binding, generation, source epoch or grant"
        );
        // Currentness is a separate binding/resource fence. A new accepted grant cannot revoke another.
        let current = self
            .registrations
            .lock()
            .map_err(poison)?
            .get(&scope_registration_key(&identity.scope))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("binding has no current live lifecycle fence"))?;
        anyhow::ensure!(
            lifecycle_matches(&current.transfer.registration, identity, require_admission)
                && current.transfer.incarnation == root.receipt.transfer.incarnation
                && current.transfer.shape == root.receipt.transfer.shape,
            "binding closed or source/version changed"
        );
        for known in self.registration_facts.lock().map_err(poison)?.values() {
            if registration_resource(known) == scope_resource(&identity.scope) {
                anyhow::ensure!(
                    !older_runtime(&root.registration, known),
                    "runtime advanced through another binding"
                );
            }
        }
        root.validate()?;
        Ok(root)
    }

    fn check_ticket(record: &Journal, request: &ConsumeExecution) -> anyhow::Result<()> {
        anyhow::ensure!(
            record.request.identity.ticket_id == request.ticket_id
                && record.request.ticket.matches(&request.ticket),
            "launch ticket does not match original execution"
        );
        Ok(())
    }

    async fn revoke(&self, request: ConsumeExecution) -> anyhow::Result<ExecutionReply> {
        let first = self.record(&request.execution_id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        let _fence = gate.lock().await;
        let mut record = self.record(&request.execution_id)?;
        Self::check_ticket(&record, &request)?;
        record.interactive_revoked = true;
        record.phase = match record.phase {
            Phase::Prepared => Phase::Revoked,
            Phase::Reserved { slot_version } => Phase::NoChild {
                slot_version,
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

    async fn control(&self, control: ExecutionControl) -> anyhow::Result<ExecutionReply> {
        let id = &control.capability.identity.execution_id;
        let first = self.record(id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
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
            ProcessAction::Observe { .. } => ExecutionControlPurpose::Observe,
            ProcessAction::WriteStdin { .. } | ProcessAction::Resize { .. } => {
                ExecutionControlPurpose::Interactive
            }
            ProcessAction::Signal(_) | ProcessAction::Cleanup => ExecutionControlPurpose::Lifecycle,
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
            self.resolve(&record.request.identity, true)?;
        }
        let process = self.processes.lock().map_err(poison)?.get(id).cloned();
        match control.action {
            ProcessAction::Observe { from_cursor } => {
                let (cursor, output) = match process {
                    Some(process) => process.output.lock().map_err(poison)?.snapshot(from_cursor),
                    None => (0, Vec::new()),
                };
                Ok(ExecutionReply::Observation {
                    receipt: Some(receipt.clone()),
                    state: record.phase.state(),
                    cursor,
                    output,
                })
            }
            ProcessAction::Cleanup => {
                anyhow::ensure!(record.durably_settled(), "execution not durably drained");
                self.processes.lock().map_err(poison)?.remove(id);
                Ok(ExecutionReply::ActionComplete)
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
                        return Ok(ExecutionReply::StdinProgress {
                            accepted_bytes: process.write(&bytes)?,
                        });
                    }
                    ProcessAction::Resize {
                        cols,
                        rows,
                        pixel_w,
                        pixel_h,
                    } => process.resize(cols, rows, pixel_w, pixel_h)?,
                    ProcessAction::Signal(signal) => process.signal(signal)?,
                    ProcessAction::Observe { .. } | ProcessAction::Cleanup => {
                        unreachable!("handled above")
                    }
                }
                Ok(ExecutionReply::ActionComplete)
            }
        }
    }

    fn monitor(
        self: &Arc<Self>,
        id: ExecutionId,
        reports: std::fs::File,
        process: Arc<process::Process>,
    ) {
        self.monitor_with_reap_barrier(id, reports, process, std::future::ready(()));
    }

    fn monitor_with_reap_barrier(
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
                                    nonce: receipt.process.nonce.clone(),
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
            std::thread::spawn(move || {
                if let Ok(mut child) = process.child.lock() {
                    let _ = child.wait();
                }
            });
            Ok::<(), anyhow::Error>(())
        });
    }

    async fn apply_event(
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
                && report.context.nonce.matches(&receipt.process.nonce)
                && report.context.mechanism == receipt.scope_mechanism,
            "helper report identity mismatch"
        );
        record.phase = match report.event {
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

    pub(super) fn legacy_allowed(&self, alias: &str) -> anyhow::Result<()> {
        let aliases = self.aliases.lock().map_err(poison)?;
        anyhow::ensure!(
            !aliases
                .get(alias)
                .is_some_and(|slot| slot.execution.is_some()),
            "typed alias requires an execution capability"
        );
        Ok(())
    }
    pub(super) fn reserve_legacy(&self, alias: &str) -> anyhow::Result<()> {
        let _persistence = self.legacy_persistence.lock().map_err(poison)?;
        let legacy = {
            let mut aliases = self.aliases.lock().map_err(poison)?;
            anyhow::ensure!(
                !aliases
                    .get(alias)
                    .is_some_and(|slot| slot.execution.is_some()),
                "typed alias cannot be used by legacy launch"
            );
            aliases.entry(alias.to_owned()).or_insert(AliasSlot {
                execution: None,
                version: 0,
            });
            aliases
                .iter()
                .filter(|(_, slot)| slot.execution.is_none())
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        self.store.write("legacy.aliases", &legacy)?;
        Ok(())
    }
}

pub fn run_scope_helper(config_fd: i32, report_fd: i32) -> anyhow::Result<()> {
    scope_helper::run(config_fd, report_fd)
}

fn reply(record: &Journal) -> ExecutionReply {
    ExecutionReply::Receipt {
        receipt: record.phase.receipt().cloned(),
        state: record.phase.state(),
    }
}
fn refused(reason: &str) -> ExecutionReply {
    ExecutionReply::Refused {
        reason: reason.into(),
    }
}
fn qualification_or_refusal(error: &anyhow::Error) -> ExecutionReply {
    match error.downcast_ref::<workspace::execution::objects::ObjectQualificationFailure>() {
        Some(failure) => ExecutionReply::ObjectQualificationFailed {
            failure: failure.clone(),
        },
        None => refused(&error.to_string()),
    }
}
fn poison<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("executor state poisoned")
}
