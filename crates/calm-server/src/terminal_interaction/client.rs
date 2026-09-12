use crate::terminal_renderer::{
    ClientInputScope, ClientPumpContext, PumpCommand, RendererEntry, run_client_pump_with_commands,
};
use anyhow::{Result, ensure};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// The most recent observation captured on a connection (any format,
/// including action readbacks). `scroll_offset` is the history offset it was
/// captured at: a history view shares the live revision but not its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatestObservation {
    pub id: Uuid,
    pub revision: u64,
    pub scroll_offset: usize,
    /// #1620 — the signal ring's `last_seq` when it was captured: the
    /// per-connection baseline for `signals.since_previous_observation` and
    /// for an observe `wait_for=signal`.
    pub last_seq: u64,
}

pub struct ScreenState {
    pub owner: Option<Uuid>,
    pub control: Option<Uuid>,
    pub available: bool,
    pub exited: bool,
    pub ack: u64,
    pub refused: u64,
    pub pending: Option<u64>,
    /// Protocol errors received on this connection and the last one's
    /// message: how a refused claim-if-unowned (#1620) is told apart from a
    /// claim that is still in flight.
    pub protocol_errors: u64,
    pub last_protocol_error: Option<String>,
}
impl ScreenState {
    fn apply(&mut self, message: DaemonMsg, id: Uuid) -> Result<()> {
        match message {
            DaemonMsg::OwnerChanged { owner_client_id } => {
                self.owner = owner_client_id;
                self.control = if self.owner == Some(id) {
                    Some(Uuid::new_v4())
                } else {
                    None
                };
            }
            DaemonMsg::InputAck { input_seq } => {
                self.ack = input_seq;
                self.pending = None;
            }
            DaemonMsg::ProtocolError { message, .. } => {
                if let Some(pending) = self.pending.take() {
                    self.refused = pending;
                }
                self.protocol_errors = self.protocol_errors.wrapping_add(1);
                self.last_protocol_error = Some(message);
            }

            DaemonMsg::TerminalExited { .. } => self.exited = true,
            _ => {}
        }
        Ok(())
    }
}

pub struct Client {
    pub binding: super::Binding,
    pub id: Uuid,
    pub connection: Uuid,
    pub entry: Arc<RendererEntry>,
    pub screen: Arc<StdMutex<ScreenState>>,
    pub serial: Mutex<()>,
    /// Inputs currently waiting for `serial` (queued behind an action still
    /// in progress on this connection). Test observability only.
    serial_waiters: AtomicUsize,
    pub requests: Mutex<std::collections::HashMap<String, (String, serde_json::Value)>>,
    pub last_used: Arc<StdMutex<std::time::Instant>>,
    pub latest_observation: StdMutex<Option<LatestObservation>>,
    incoming: mpsc::Sender<ClientMsg>,
    commands: mpsc::Sender<PumpCommand>,
    changed: watch::Receiver<u64>,
    pump: JoinHandle<anyhow::Result<()>>,
    reader: JoinHandle<()>,
}
pub struct SerialQueued<'a>(&'a AtomicUsize);
impl Drop for SerialQueued<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.pump.abort();
        self.reader.abort();
    }
}
impl Client {
    pub async fn attach(
        entry: Arc<RendererEntry>,
        scope: ClientInputScope,
        binding: super::Binding,
    ) -> Result<Self> {
        let id = Uuid::new_v4();
        let size = entry
            .handle
            .render_plane
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal renderer poisoned"))?
            .current_size();
        let (incoming, incoming_rx) = mpsc::channel(8);
        let (commands, commands_rx) = mpsc::channel(1);
        let (outgoing_tx, mut outgoing) = mpsc::channel(128);
        let pump = tokio::spawn(run_client_pump_with_commands(
            incoming_rx,
            Some(commands_rx),
            outgoing_tx,
            ClientPumpContext {
                input_barrier: entry.handle.input_barrier.clone(),
                input_scope: scope.clone(),
                event_rx: entry.subscribe(),
                event_tx: entry.handle.event_tx.clone(),
                render_plane: entry.handle.render_plane.clone(),
                exit: entry.exit.clone(),
                supervisor_tx: entry.handle.supervisor_tx.clone(),
                owner_registry: entry.handle.owner_registry.clone(),
                session_id: entry.handle.session_id,
                terminal_id: entry.terminal_id.clone(),
            },
        ));
        // Abort the actual connection if attachment fails before ownership moves.
        struct Abort(Option<tokio::task::AbortHandle>);
        impl Drop for Abort {
            fn drop(&mut self) {
                if let Some(handle) = &self.0 {
                    handle.abort();
                }
            }
        }
        let mut abort = Abort(Some(pump.abort_handle()));
        incoming
            .send(ClientMsg::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                terminal_id: entry.terminal_id.clone(),
                client_id: id,
                desired_size: PtySize {
                    cols: size.cols,
                    rows: size.rows,
                    pixel_width: None,
                    pixel_height: None,
                },
                cell_size: None,
                initial_scrollback: InitialScrollback::All,
                resume_from: None,
                role_hint: Some(Role::Observer),
                capabilities: ClientCapabilities {
                    render_encodings: vec![RenderEncoding::Vt],
                    supports_scrollback: true,
                    supports_sixel: false,
                    supports_images: false,
                    kernel_originated_input: false,
                },
            })
            .await?;
        let hello = tokio::time::timeout(Duration::from_secs(5), outgoing.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("terminal handshake closed"))?;
        let DaemonMsg::ServerHello {
            owner_client_id, ..
        } = hello
        else {
            anyhow::bail!("terminal handshake refused");
        };
        let state = ScreenState {
            owner: owner_client_id,
            control: None,
            available: true,
            exited: false,
            ack: 0,
            refused: 0,
            pending: None,
            protocol_errors: 0,
            last_protocol_error: None,
        };
        let screen = Arc::new(StdMutex::new(state));
        let (notify, changed) = watch::channel(0u64);
        let reader_screen = screen.clone();
        let last_used = Arc::new(StdMutex::new(std::time::Instant::now()));
        let reader_last_used = last_used.clone();
        let pump_abort = pump.abort_handle();
        let reader = tokio::spawn(async move {
            let mut check = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    message = outgoing.recv() => {
                        let Some(message) = message else { break; };
                        let Ok(mut state) = reader_screen.lock() else { break; };
                        if state.apply(message,id).is_err() { state.available=false; }
                        notify.send_modify(|sequence| *sequence=sequence.wrapping_add(1));
                    }
                    _ = check.tick() => {
                        let idle = reader_last_used.lock().map(|last|last.elapsed()>Duration::from_secs(600)).unwrap_or(true);
                        if idle || !scope.allowed().await { break; }
                    }
                }
            }
            pump_abort.abort();
            if let Ok(mut state) = reader_screen.lock() {
                state.available = false;
                state.control = None;
            }
            notify.send_modify(|sequence| *sequence = sequence.wrapping_add(1));
        });
        abort.0.take();
        Ok(Self {
            binding,
            id,
            connection: Uuid::new_v4(),
            entry,
            screen,
            serial: Mutex::new(()),
            serial_waiters: AtomicUsize::new(0),
            requests: Mutex::new(std::collections::HashMap::new()),
            last_used,
            latest_observation: StdMutex::new(None),
            incoming,
            commands,
            changed,
            pump,
            reader,
        })
    }
    pub async fn wait(
        &self,
        predicate: impl Fn(&ScreenState) -> bool,
        budget: Duration,
    ) -> Result<()> {
        let mut changed = self.changed.clone();
        tokio::time::timeout(budget, async {
            loop {
                changed.borrow_and_update();
                {
                    let state = self
                        .screen
                        .lock()
                        .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
                    if predicate(&state) {
                        return Ok::<_, anyhow::Error>(());
                    }
                    ensure!(state.available, "terminal disconnected");
                }
                changed.changed().await?;
            }
        })
        .await?
    }
    /// Count this input as waiting for `serial` until the guard is dropped
    /// (once the lock is held, or when the call is cancelled while queued).
    pub fn queued_for_serial(&self) -> SerialQueued<'_> {
        self.serial_waiters.fetch_add(1, Ordering::SeqCst);
        SerialQueued(&self.serial_waiters)
    }
    pub fn serial_waiters(&self) -> usize {
        self.serial_waiters.load(Ordering::SeqCst)
    }
    /// Wakes on every protocol message (ownership, ack/refusal, exit) and on
    /// disconnect; the model-view revision channel covers screen output.
    pub fn changed(&self) -> watch::Receiver<u64> {
        self.changed.clone()
    }
    pub async fn send(&self, message: ClientMsg) -> Result<()> {
        self.incoming.send(message).await.map_err(Into::into)
    }
    /// #1620 — ask the pump to claim control only if no other client holds
    /// it (decided under the owner-registry lock). The outcome arrives as an
    /// `OwnerChanged` naming this client or as a protocol error.
    pub async fn claim_if_unowned(&self) -> Result<()> {
        self.commands
            .send(PumpCommand::ClaimIfUnowned)
            .await
            .map_err(Into::into)
    }
}
