//! Scripted port fixture and shared executor contract, available only by opt-in.
use super::*;
mod store_contract;
use async_trait::async_trait;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
pub use store_contract::StoreHarness;
use tokio::sync::{Notify, watch};
use workspace::identity::WorkspaceCwd;

#[derive(Clone, Default)]
pub struct CallLog(Arc<Mutex<Vec<String>>>);
impl CallLog {
    pub fn push(&self, call: &str) {
        self.0.lock().unwrap().push(call.into());
    }
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}
/// A controllable await point. Tests arrange schedules without sleeps.
#[derive(Default)]
pub struct Pause {
    pub entered: Notify,
    pub resume: Notify,
}
impl Pause {
    pub async fn stop(&self) {
        self.entered.notify_one();
        self.resume.notified().await;
    }
}
#[derive(Clone)]
pub struct ScriptedExecutor {
    pub log: CallLog,
    script: Arc<Mutex<VecDeque<Result<Fact, ExecError>>>>,
    changes: watch::Sender<u64>,
    pub inspect_pause: Arc<Mutex<Option<Arc<Pause>>>>,
    pub retire_pause: Arc<Mutex<Option<Arc<Pause>>>>,
    pub generation_pause: Arc<Mutex<Option<Arc<Pause>>>>,
}
impl ScriptedExecutor {
    pub fn new(log: CallLog, script: impl IntoIterator<Item = Result<Fact, ExecError>>) -> Self {
        Self {
            log,
            script: Arc::new(Mutex::new(script.into_iter().collect())),
            changes: watch::channel(0).0,
            retire_pause: Default::default(),
            inspect_pause: Default::default(),
            generation_pause: Default::default(),
        }
    }
    pub fn changed(&self) {
        self.changes.send_modify(|g| *g += 1);
    }
    pub fn push(&self, fact: Result<Fact, ExecError>) {
        self.script.lock().unwrap().push_back(fact);
    }
    fn effect(&self, call: &str) -> Result<Fact, ExecError> {
        let result = self.fact(call);
        if result.is_ok() {
            self.changed();
        }
        result
    }
    fn fact(&self, call: &str) -> Result<Fact, ExecError> {
        self.log.push(call);
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected executor call")
    }
}
#[async_trait]
impl Executor for ScriptedExecutor {
    async fn prepare(&self, _: &ExecutionIntent) -> Result<Fact, ExecError> {
        self.effect("prepare")
    }
    async fn launch(&self, _: &ConsumeExecution) -> Result<Fact, ExecError> {
        self.effect("launch")
    }
    async fn revoke(&self, _: &ConsumeExecution) -> Result<Fact, ExecError> {
        self.effect("revoke")
    }
    async fn inspect(&self, _: &ExecutionId) -> Result<Fact, ExecError> {
        let fact = self.fact("inspect");
        let pause = self.inspect_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.stop().await;
        }
        fact
    }
    async fn generation(&self) -> Result<Generation, ExecError> {
        let generation = Generation::snapshot(self.changes.subscribe());
        let pause = self.generation_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.stop().await;
        }
        Ok(generation)
    }
    fn available(&self) -> bool {
        true
    }
    async fn control(&self, _: ExecutionControl) -> Result<ExecutionReply, ExecError> {
        self.fact("control").map(|_| ExecutionReply::ActionComplete)
    }
    async fn retire(&self, _: &ExecutionId, _: &CapabilityToken) -> Result<Fact, ExecError> {
        let result = self.effect("retire");
        let pause = self.retire_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.stop().await;
        }
        result
    }
}
/// Transaction fixture: writes remain invisible until the test explicitly commits.
#[derive(Default)]
pub struct MemoryTx(Vec<(ExecutionId, Stored)>);
#[derive(Clone, Default)]
pub struct MemoryStore {
    rows: Arc<Mutex<HashMap<ExecutionId, Stored>>>,
    pub log: CallLog,
    pub load_pause: Arc<Mutex<Option<Arc<Pause>>>>,
    pub settle_pause: Arc<Mutex<Option<Arc<Pause>>>>,
}
impl MemoryStore {
    pub fn new(log: CallLog) -> Self {
        Self {
            log,
            ..Self::default()
        }
    }
    pub fn commit(&self, tx: MemoryTx) {
        let mut rows = self.rows.lock().unwrap();
        for (id, row) in tx.0 {
            rows.insert(id, row);
        }
    }
    pub fn seed(&self, intent: ExecutionIntent, desired: Desired) {
        self.rows.lock().unwrap().insert(
            intent.metadata.id.clone(),
            Stored::Active {
                intent: intent.metadata,
                secrets: intent.secrets,
                desired,
                settlement: None,
            },
        );
    }
}
#[async_trait]
impl Store for MemoryStore {
    type Tx<'t> = MemoryTx;
    async fn insert(
        &self,
        tx: &mut Self::Tx<'_>,
        intent: &ExecutionIntent,
    ) -> Result<(), StoreError> {
        tx.0.push((
            intent.metadata.id.clone(),
            Stored::Active {
                intent: intent.metadata.clone(),
                secrets: intent.secrets.clone(),
                desired: Desired::Run,
                settlement: None,
            },
        ));
        Ok(())
    }
    async fn request(
        &self,
        tx: &mut Self::Tx<'_>,
        id: &ExecutionId,
        desired: Desired,
    ) -> Result<(), StoreError> {
        let mut row =
            tx.0.iter()
                .rev()
                .find(|(key, _)| key == id)
                .map(|(_, row)| row.clone())
                .or_else(|| self.rows.lock().unwrap().get(id).cloned())
                .ok_or_else(|| StoreError("missing intent".into()))?;
        if let Stored::Active {
            desired: current, ..
        } = &mut row
        {
            *current = (*current).max(desired);
        }
        tx.0.push((id.clone(), row));
        Ok(())
    }
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError> {
        self.log.push("load");
        let row = self.rows.lock().unwrap().get(id).cloned();
        let pause = self.load_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.stop().await;
        }
        Ok(row)
    }
    async fn spawned(&self, _: &ExecutionId) -> Result<(), StoreError> {
        self.log.push("spawned");
        Ok(())
    }
    async fn settle(&self, id: &ExecutionId, evidence: &Settlement) -> Result<(), StoreError> {
        self.log.push("settle");
        if let Stored::Active { settlement, .. } = self.rows.lock().unwrap().get_mut(id).unwrap()
            && settlement.is_none()
        {
            *settlement = Some(evidence.clone());
        }
        let pause = self.settle_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.stop().await;
        }
        Ok(())
    }
    async fn finish(&self, id: &ExecutionId) -> Result<(), StoreError> {
        self.log.push("finish");
        let mut rows = self.rows.lock().unwrap();
        let row = rows.get_mut(id).unwrap();
        if let Stored::Active {
            intent, settlement, ..
        } = row
        {
            *row = Stored::Finished {
                intent: intent.clone(),
                settlement: settlement
                    .clone()
                    .ok_or_else(|| StoreError("finish before settlement".into()))?,
            };
        }
        Ok(())
    }
    async fn pending(&self) -> Result<Vec<ExecutionId>, StoreError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| {
                matches!(r, Stored::Active { desired, settlement, .. }
                if settlement.is_none() || *desired == Desired::Retire)
            })
            .map(|(id, _)| id.clone())
            .collect())
    }
}
pub fn intent(root: std::path::PathBuf) -> ExecutionIntent {
    ExecutionIntent {
        metadata: ExecutionMetadata {
            id: ExecutionId::fresh().unwrap(),
            alias: ProcessAlias(ExecutionId::fresh().unwrap().as_str().into()),
            owner: OperationRef("port-contract".into()),
            track: TrackId("test-track".into()),
            source: PrivateSource {
                root,
                cwd: WorkspaceCwd::root(),
                registration: ObjectRegistrationId::fresh().unwrap(),
                nonce: TransferNonce::fresh().unwrap(),
                directory: ExternalDirectoryId::fresh().unwrap(),
                incarnation: ExternalDirectoryIncarnationId::fresh().unwrap(),
                authority: ExecutionAuthority {
                    grant_id: ExecutionGrantId::fresh().unwrap(),
                    authorization_binding: AuthorizationBindingId::fresh().unwrap(),
                    authorization_version: 1,
                },
            },
            program: ExecutionSpecification {
                program: "/bin/true".into(),
                args: vec![],
                environment: vec![],
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 1024,
            },
            specification: SpecificationId::fresh().unwrap(),
            ticket_id: LaunchTicketId::fresh().unwrap(),
        },
        secrets: ExecutionSecrets {
            ticket: CapabilityToken::fresh().unwrap(),
            tokens: ExecutionControlTokens::fresh().unwrap(),
        },
    }
}
pub fn journal(state: ExecutionState, output: OutputEnd) -> Fact {
    Fact::Journal {
        state,
        receipt: None,
        output,
    }
}
pub fn tombstone(intent: &ExecutionIntent) -> Fact {
    Fact::Retired(ExecutionTombstone {
        execution_id: intent.metadata.id.clone(),
        ticket_id: intent.metadata.ticket_id.clone(),
        registration: None,
        directory: None,
        slot: None,
        outcome: TombstoneOutcome::Revoked,
    })
}
/// Shared assertions against a caller-supplied port. The fake supplies replies, not policy.
pub mod port_contract {
    pub use super::store_contract::store;
    use super::*;
    pub async fn executor<X: Executor>(
        executor: &X,
        intent: &ExecutionIntent,
        launch: &ExecutionIntent,
    ) {
        let generation = executor.generation().await.unwrap();
        assert!(executor.available());
        assert_eq!(
            executor.inspect(&intent.metadata.id).await.unwrap(),
            Fact::Absent
        );
        let retired = executor.revoke(&intent.consume()).await.unwrap();
        let Fact::Retired(t) = &retired else {
            panic!("unknown revoke must tombstone")
        };
        assert_eq!(t.execution_id, intent.metadata.id);
        assert_eq!(t.ticket_id, intent.metadata.ticket_id);
        assert_eq!(t.outcome, TombstoneOutcome::Revoked);
        tokio::time::timeout(std::time::Duration::from_secs(2), generation.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            executor.inspect(&intent.metadata.id).await.unwrap(),
            retired
        );
        assert_eq!(executor.revoke(&intent.consume()).await.unwrap(), retired);
        assert!(matches!(
            executor.prepare(intent).await,
            Err(ExecError::Refused(_))
        ));
        assert_eq!(
            executor
                .retire(&intent.metadata.id, &intent.secrets.tokens.lifecycle)
                .await
                .unwrap(),
            retired
        );

        assert_eq!(
            executor.inspect(&launch.metadata.id).await.unwrap(),
            Fact::Absent
        );
        let prepared = executor.prepare(launch).await.unwrap();
        assert!(matches!(
            prepared,
            Fact::Journal {
                state: ExecutionState::Prepared,
                ..
            }
        ));
        assert_eq!(executor.prepare(launch).await.unwrap(), prepared);
        assert_eq!(
            executor
                .retire(&launch.metadata.id, &launch.secrets.tokens.lifecycle)
                .await,
            Err(ExecError::Busy(BusyReason::OutputOpen))
        );
        let fact = executor.launch(&launch.consume()).await.unwrap();
        let Fact::Journal {
            receipt: Some(receipt),
            ..
        } = fact
        else {
            panic!("launch must preserve process receipt")
        };
        assert_eq!(receipt.identity.execution_id, launch.metadata.id);
        let control = ExecutionControl {
            capability: ExecutionCapability {
                identity: receipt.identity.clone(),
                slot_version: receipt.slot_version,
                token: launch.secrets.tokens.lifecycle.clone(),
                purpose: ExecutionControlPurpose::Lifecycle,
            },
            action: ProcessAction::Stop,
        };
        assert!(matches!(
            executor.control(control).await.unwrap(),
            ExecutionReply::ActionComplete
        ));
        loop {
            let generation = executor.generation().await.unwrap();
            let fact = executor.inspect(&launch.metadata.id).await.unwrap();
            if matches!(
                fact,
                Fact::Journal {
                    state: ExecutionState::Drained { .. },
                    output: OutputEnd::Complete { .. },
                    ..
                }
            ) {
                break;
            }
            tokio::time::timeout(std::time::Duration::from_secs(3), generation.changed())
                .await
                .unwrap()
                .unwrap();
        }
        let retired = executor
            .retire(&launch.metadata.id, &launch.secrets.tokens.lifecycle)
            .await
            .unwrap();
        let Fact::Retired(t) = &retired else {
            panic!("retire must tombstone")
        };
        assert_eq!(t.execution_id, launch.metadata.id);
        assert_eq!(t.outcome, TombstoneOutcome::Retired);
        assert_eq!(
            executor.inspect(&launch.metadata.id).await.unwrap(),
            retired
        );
        assert_eq!(
            executor
                .retire(&launch.metadata.id, &launch.secrets.tokens.lifecycle)
                .await
                .unwrap(),
            retired
        );
        assert!(matches!(
            executor.prepare(launch).await,
            Err(ExecError::Refused(_))
        ));
    }
}
