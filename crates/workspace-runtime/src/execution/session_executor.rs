//! Thin v4 protocol adapter. A dedicated Watch stream wakes all execution drivers.
use super::*;
use async_trait::async_trait;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, watch};
use workspace::execution::objects::{HeldSourceObjects, ObjectRole};

#[derive(Clone)]
pub struct SessionExecutor(Arc<Adapter>);
struct Adapter {
    socket: PathBuf,
    secret: CapabilityToken,
    generation: watch::Receiver<u64>,
    watching: Arc<AtomicBool>,
    reconnect: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Adapter {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl SessionExecutor {
    /// Starts the sole Watch connection. No execution state is cached here.
    pub fn new(socket: PathBuf, secret: CapabilityToken) -> Self {
        let (send, generation) = watch::channel(0);
        let watching = Arc::new(AtomicBool::new(false));
        let reconnect = Arc::new(Notify::new());
        let restart = reconnect.clone();
        let (path, key, ready) = (socket.clone(), secret.clone(), watching.clone());
        let task = tokio::spawn(async move {
            loop {
                let connection = RegisteredExecutorConnection::connect(&path, key.clone()).await;
                if let Ok(connection) = connection
                    && let Ok(mut stream) = connection.watch().await
                {
                    ready.store(true, Ordering::Release);
                    send.send_modify(|g| *g += 1); // Watching, including reconnect/boot change
                    loop {
                        tokio::select! {
                            changed = stream.changed() => {
                                if changed.is_err() { break; }
                                send.send_modify(|g| *g += 1);
                            }
                            _ = restart.notified() => break,
                        }
                    }
                }
                ready.store(false, Ordering::Release);
                send.send_modify(|g| *g += 1); // loss also wakes every waiter
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        Self(Arc::new(Adapter {
            socket,
            secret,
            generation,
            watching,
            reconnect,
            task,
        }))
    }
    async fn connect(&self) -> Result<RegisteredExecutorConnection, ExecError> {
        RegisteredExecutorConnection::connect(&self.0.socket, self.0.secret.clone())
            .await
            .map_err(|error| self.transport(error))
    }
    async fn request(&self, request: ExecutionRequest) -> Result<ExecutionReply, ExecError> {
        checked(
            self.connect()
                .await?
                .request(request)
                .await
                .map_err(|error| self.transport(error))?,
        )
    }
    fn transport(&self, error: std::io::Error) -> ExecError {
        // A lost command need not close Watch or produce Changed. Restart Watch so
        // every pending driver gets a new Watching generation before re-inspecting.
        self.0.reconnect.notify_one();
        ExecError::from(error)
    }
    async fn effect(&self, id: &ExecutionId, request: ExecutionRequest) -> Result<Fact, ExecError> {
        let reply = self.request(request).await?;
        match reply {
            ExecutionReply::Retired(t) => Ok(Fact::Retired(t)),
            _ => self.inspect(id).await,
        }
    }
}
fn checked(reply: ExecutionReply) -> Result<ExecutionReply, ExecError> {
    match reply {
        ExecutionReply::Busy(reason) => Err(ExecError::Busy(reason)),
        ExecutionReply::Refused { reason } => Err(ExecError::Refused(reason)),
        ExecutionReply::ObjectQualificationFailed { failure } => {
            Err(ExecError::Refused(format!("{failure:?}")))
        }
        reply => Ok(reply),
    }
}
#[async_trait]
impl Executor for SessionExecutor {
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError> {
        let mut connection = self.connect().await?;
        let source = &intent.metadata.source;
        let held = HeldSourceObjects::open(&source.root, false)
            .map_err(|e| ExecError::Refused(e.to_string()))?;
        // Device/inode are wire inputs from the held object's reported facts, never settlement.
        let root = held
            .shape()
            .objects
            .iter()
            .find(|f| f.role == ObjectRole::Root)
            .ok_or_else(|| ExecError::Refused("held source has no root".into()))?;
        let incarnation = SourceObjectIncarnation::ExternalDir(source.incarnation.clone());
        let boot = connection.executor_boot().clone();
        let request = PrepareExecution {
            source: PreparedSource::Private(Box::new(AttachRegistration {
                registration_id: source.registration.clone(),
                executor_boot: boot.clone(),
                nonce: source.nonce.clone(),
                incarnation: incarnation.clone(),
                shape: held.shape().clone(),
                registration: ExecutionRegistration::ExternalDir {
                    directory_id: source.directory.clone(),
                    root: DirectoryRegistration {
                        path: source.root.clone(),
                        device: root.device,
                        inode: root.inode,
                    },
                    registration_version: 1,
                    admission_open: true,
                    authority: source.authority.clone(),
                },
            })),
            identity: ExecutionIdentity {
                execution_id: intent.metadata.id.clone(),
                executor_boot: boot,
                ticket_id: intent.metadata.ticket_id.clone(),
                source_registration: source.registration.clone(),
                source_incarnation: incarnation,
                authority: source.authority.clone(),
                scope: ExecutionScope::ExternalDir {
                    directory_id: source.directory.clone(),
                    registration_version: 1,
                    cwd: source.cwd.clone(),
                },
                operation_id: intent.metadata.owner.0.clone(),
                specification_id: intent.metadata.specification.clone(),
                process_alias: intent.metadata.alias.0.clone(),
            },
            ticket: intent.secrets.ticket.clone(),
            control_tokens: intent.secrets.tokens.clone(),
            handoff: None,
            specification: intent.metadata.program.clone(),
        };
        checked(
            connection
                .prepare_private(request, &held)
                .await
                .map_err(|error| self.transport(error))?,
        )?;
        self.inspect(&intent.metadata.id).await
    }
    async fn launch(&self, consume: &ConsumeExecution) -> Result<Fact, ExecError> {
        self.effect(
            &consume.execution_id,
            ExecutionRequest::Launch(consume.clone()),
        )
        .await
    }
    async fn revoke(&self, consume: &ConsumeExecution) -> Result<Fact, ExecError> {
        self.effect(
            &consume.execution_id,
            ExecutionRequest::Revoke(consume.clone()),
        )
        .await
    }
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError> {
        match self.request(ExecutionRequest::Inspect(id.clone())).await? {
            ExecutionReply::Absent => Ok(Fact::Absent),
            ExecutionReply::Retired(t) => Ok(Fact::Retired(t)),
            ExecutionReply::Inspected {
                state,
                receipt,
                output_end,
            } => Ok(Fact::Journal {
                state,
                receipt: receipt.map(Box::new),
                output: output_end,
            }),
            _ => Err(self.transport(std::io::Error::other("unexpected Inspect response"))),
        }
    }
    async fn generation(&self) -> Result<Generation, ExecError> {
        let mut receiver = self.0.generation.clone();
        loop {
            let generation = Generation::snapshot(receiver.clone());
            if self.available() {
                return Ok(generation);
            }
            receiver
                .changed()
                .await
                .map_err(|_| ExecError::Transport("Watch task stopped".into()))?;
        }
    }
    fn available(&self) -> bool {
        self.0.watching.load(Ordering::Acquire)
    }
    async fn control(&self, control: ExecutionControl) -> Result<ExecutionReply, ExecError> {
        self.request(ExecutionRequest::Control(control)).await
    }
    async fn retire(
        &self,
        id: &ExecutionId,
        lifecycle: &CapabilityToken,
    ) -> Result<Fact, ExecError> {
        self.effect(
            id,
            ExecutionRequest::Retire {
                execution_id: id.clone(),
                lifecycle: lifecycle.clone(),
            },
        )
        .await
    }
}
