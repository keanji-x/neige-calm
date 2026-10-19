//! Durable executor mechanisms. Authorization and scheduling decisions remain in kernel.

mod attachment;
mod controls;
mod directory;
mod journal;
mod launch;
mod monitor;
#[cfg(test)]
mod monitor_tests;
mod native_child;
mod output;
mod preparation;
mod private;
mod process;
mod protocol;
#[cfg(test)]
mod protocol_tests;
mod retirement;
mod scope_helper;
mod shutdown;
#[cfg(test)]
mod stop_tests;
#[cfg(test)]
mod tombstone_tests;
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
    host_boot_id: String,
    helper_image: std::fs::File,
    mechanism: ScopeMechanismIdentity,
    legacy_persistence: StdMutex<()>,
    id_gates: StdMutex<HashMap<ExecutionId, Arc<Mutex<()>>>>,
    tombstones: StdMutex<HashMap<ExecutionId, ExecutionTombstone>>,
    changes: StdMutex<protocol::Changes>,
    gates: StdMutex<HashMap<ResourceKey, Arc<Mutex<()>>>>,
    registrations: StdMutex<HashMap<String, ObjectRegistrationReceipt>>,
    // Latest per-binding source facts across boots; these never restore a live native lease.
    registration_facts: StdMutex<HashMap<String, AttachRegistration>>,
    transfers: StdMutex<HashMap<TransferNonce, ObjectRegistrationReceipt>>,
    live_objects: StdMutex<HashMap<ObjectRegistrationId, Arc<RegisteredRoot>>>,
    retired: StdMutex<HashMap<ObjectRegistrationId, ObjectRegistrationReceipt>>,
    preparations: preparation::PreparationReservations,
    records: StdMutex<HashMap<ExecutionId, Journal>>,
    aliases: StdMutex<HashMap<String, AliasSlot>>,
    processes: StdMutex<HashMap<ExecutionId, Arc<process::Process>>>,
    outputs: StdMutex<HashMap<ExecutionId, Arc<output::OutputLog>>>,
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
        let host_boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        anyhow::ensure!(!host_boot_id.is_empty(), "missing host boot identity");
        let mut outputs = HashMap::new();
        let mut records = HashMap::new();
        let mut aliases: HashMap<String, AliasSlot> = HashMap::new();
        let tombstones = store.tombstones()?;
        for tombstone in tombstones.values() {
            Self::replay_retired(&store, tombstone)?;
            if let Some((alias, version)) = &tombstone.slot {
                let slot = aliases.entry(alias.clone()).or_insert(AliasSlot {
                    execution: Some(tombstone.execution_id.clone()),
                    version: *version,
                });
                anyhow::ensure!(
                    slot.version != *version
                        || slot.execution.as_ref() == Some(&tombstone.execution_id),
                    "conflicting tombstone alias slots"
                );
                if *version > slot.version {
                    *slot = AliasSlot {
                        execution: Some(tombstone.execution_id.clone()),
                        version: *version,
                    };
                }
            }
        }
        for mut record in store.load()? {
            if tombstones.contains_key(&record.request.identity.execution_id) {
                continue;
            }
            let identity = record.request.identity.clone();
            if let Some(receipt) = record.phase.receipt() {
                anyhow::ensure!(
                    receipt.identity == identity,
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
            let path = store.output_path(&identity.execution_id);
            if record.phase.receipt().is_some() || path.exists() {
                // Persisted completion must agree with the actual durable log.
                let log =
                    output::OutputLog::open(&path, record.request.specification.replay_bytes)?;
                if let OutputEnd::Complete { final_cursor } = record.output_end
                    && log.snapshot(None)?.0 != final_cursor
                {
                    record.output_end = OutputEnd::Incomplete {
                        reason: "output_log_length_mismatch".into(),
                    };
                }
                outputs.insert(identity.execution_id.clone(), log);
            }
            let rebooted = record.host_boot_id != host_boot_id;
            // A formerly possible child can have output even when reboot maps it to NoChild.
            if rebooted
                && !matches!(
                    record.phase,
                    Phase::Prepared
                        | Phase::Reserved { .. }
                        | Phase::Revoked
                        | Phase::NoChild { .. }
                )
                && record.output_end == OutputEnd::Open
            {
                record.output_end = OutputEnd::Incomplete {
                    reason: "host_rebooted".into(),
                };
            }
            record.phase = match &record.phase {
                phase if record.host_boot_id != host_boot_id && !phase.state().permits_handoff() => Phase::NoChild {
                    slot_version: phase.slot_version(), reason: "host_rebooted".into()
                },
                Phase::Prepared => { record.interactive_revoked = true; Phase::Revoked },
                Phase::Reserved { slot_version } => Phase::NoChild { slot_version: Some(*slot_version), reason: "executor restarted before fork barrier".into() },
                phase if !phase.state().permits_handoff() => Phase::Unknown { slot_version: phase.slot_version().ok_or_else(|| anyhow::anyhow!("missing active alias version"))?, receipt: phase.receipt().cloned(), reason: "executor restarted without original native handles; reconciliation required".into() },
                phase => phase.clone(),
            };
            record.settle_unspawned_output();
            if record.output_end == OutputEnd::Open {
                record.output_end = OutputEnd::Incomplete {
                    reason: if rebooted {
                        "host_rebooted"
                    } else {
                        "executor_restarted"
                    }
                    .into(),
                };
            }
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
        let mut registration_facts = HashMap::<String, AttachRegistration>::new();
        let mut transfers = HashMap::new();
        // Serialized receipts retain audit/version facts only. No descriptor survives this boot.
        for receipt in store.registrations()? {
            anyhow::ensure!(
                receipt.contract == PERSISTED_RECEIPT_CONTRACT_VERSION
                    && receipt.id == receipt.transfer.registration_id,
                "unsupported persisted attachment contract or provenance"
            );
            let registration = &receipt.transfer.registration;
            let key = registration_key(registration);
            if let Some(previous) = registration_facts.get(&key) {
                if source_successor(previous, &receipt.transfer) {
                    registration_facts.insert(key, receipt.transfer.clone());
                } else {
                    anyhow::ensure!(
                        source_successor(&receipt.transfer, previous),
                        "conflicting persisted source registrations"
                    );
                }
            } else {
                registration_facts.insert(key, receipt.transfer.clone());
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
            host_boot_id,
            helper_image,
            mechanism,
            legacy_persistence: Default::default(),
            gates: Default::default(),
            id_gates: Default::default(),
            tombstones: StdMutex::new(tombstones),
            changes: Default::default(),
            registrations: Default::default(),
            registration_facts: StdMutex::new(registration_facts),
            transfers: StdMutex::new(transfers),
            live_objects: Default::default(),
            retired: StdMutex::new(retired),
            preparations: preparation::PreparationReservations::from_records(&records)?,
            records: StdMutex::new(records),
            aliases: StdMutex::new(aliases),
            processes: Default::default(),
            outputs: StdMutex::new(outputs),
        });
        executor.store.write("executor.boot", &executor.boot)?;
        let ids = executor
            .records
            .lock()
            .map_err(poison)?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            executor.release_private(&id)?;
        }
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
            Err(error) => refused(&error.to_string()),
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
            ExecutionRequest::Attach(_) => {
                anyhow::bail!("object attachment requires the controlled ancillary transport")
            }
            ExecutionRequest::RetireRegistration(receipt) => {
                self.retire_registration(receipt).await
            }
            ExecutionRequest::Prepare(request) => self.prepare(request).await,
            ExecutionRequest::Inspect(id) => self.inspect(&id),
            ExecutionRequest::Watch => Ok(self.watch()?.0),
            ExecutionRequest::Retire {
                execution_id,
                lifecycle,
            } => self.retire_execution(execution_id, lifecycle).await,
            ExecutionRequest::Launch(request) => self.launch(request).await,
            ExecutionRequest::Revoke(request) => self.revoke(request).await,
            ExecutionRequest::Control(control) => self.control(control).await,
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
        record.settle_unspawned_output();
        self.store.save(&record)?;
        record.pending_persistence = false;
        let id = record.request.identity.execution_id.clone();
        self.records
            .lock()
            .map_err(poison)?
            .insert(id.clone(), record);
        self.release_private(&id)?;
        self.changed(id)?;
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
        record.settle_unspawned_output();
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
            if registration_resource(&known.registration) == scope_resource(&identity.scope) {
                anyhow::ensure!(
                    !older_runtime(&root.registration, &known.registration),
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
fn poison<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("executor state poisoned")
}
