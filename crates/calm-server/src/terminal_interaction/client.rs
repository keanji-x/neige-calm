use crate::terminal_renderer::{
    ClientInputScope, ClientPumpContext, RendererEntry, run_client_pump,
};
use anyhow::{Result, ensure};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, RenderSnapshot, Role,
};
use calm_terminal_view::{Frame, TerminalView};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub struct ScreenState {
    view: TerminalView,
    pub revision: u32,
    pub output_sequence: u32,
    pub owner: Option<Uuid>,
    pub control: Option<Uuid>,
    pub available: bool,
    pub exited: bool,
    pub ack: u64,
    pub refused: u64,
    pub pending: Option<u64>,
}
impl ScreenState {
    pub fn frame(&self, offset: usize) -> Result<Frame> {
        ensure!(
            self.available,
            "terminal observation unavailable; reconnect explicitly"
        );
        self.view.frame(offset)
    }
    fn snapshot(&mut self, snapshot: RenderSnapshot, fg: [u8; 3], bg: [u8; 3]) -> Result<()> {
        ensure!(
            snapshot.encoding == RenderEncoding::Vt,
            "unsupported terminal encoding"
        );
        self.view = TerminalView::new(snapshot.cols, snapshot.rows, fg, bg)?;
        if let Some(history) = snapshot.scrollback {
            self.view.feed(&history);
        }
        self.view.feed(&snapshot.data);
        self.revision = snapshot.render_rev;
        self.output_sequence = snapshot.pty_seq;
        self.available = true;
        Ok(())
    }
    fn apply(&mut self, message: DaemonMsg, id: Uuid, fg: [u8; 3], bg: [u8; 3]) -> Result<()> {
        match message {
            DaemonMsg::RenderPatch(patch) => {
                if patch.pty_seq <= self.output_sequence {
                    return Ok(());
                }
                ensure!(
                    patch.prev_render_rev == self.revision
                        && patch.encoding == RenderEncoding::Vt
                        && self.output_sequence.checked_add(1) == Some(patch.pty_seq),
                    "terminal output gap"
                );
                self.view.feed(&patch.data);
                self.revision = patch.render_rev;
                self.output_sequence = patch.pty_seq;
            }
            DaemonMsg::RenderSnapshot(snapshot) => self.snapshot(snapshot, fg, bg)?,
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
            DaemonMsg::ProtocolError { .. } => {
                if let Some(pending) = self.pending.take() {
                    self.refused = pending;
                }
            }
            DaemonMsg::SnapshotRequired { .. } => self.available = false,
            DaemonMsg::TerminalExited { .. } => self.exited = true,
            _ => {}
        }
        Ok(())
    }
}

pub struct Client {
    pub id: Uuid,
    pub connection: Uuid,
    pub entry: Arc<RendererEntry>,
    pub screen: Arc<StdMutex<ScreenState>>,
    pub serial: Mutex<()>,
    incoming: mpsc::Sender<ClientMsg>,
    changed: watch::Receiver<u64>,
    pump: JoinHandle<anyhow::Result<()>>,
    reader: JoinHandle<()>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.pump.abort();
        self.reader.abort();
    }
}
impl Client {
    pub async fn attach(entry: Arc<RendererEntry>, scope: ClientInputScope) -> Result<Self> {
        let id = Uuid::new_v4();
        let config = entry.config();
        let fg = [
            config.terminal_fg.0,
            config.terminal_fg.1,
            config.terminal_fg.2,
        ];
        let bg = [
            config.terminal_bg.0,
            config.terminal_bg.1,
            config.terminal_bg.2,
        ];
        let size = entry
            .handle
            .render_plane
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal renderer poisoned"))?
            .current_size();
        let (incoming, incoming_rx) = mpsc::channel(8);
        let (outgoing_tx, mut outgoing) = mpsc::channel(128);
        let pump = tokio::spawn(run_client_pump(
            incoming_rx,
            outgoing_tx,
            ClientPumpContext {
                input_barrier: entry.handle.input_barrier.clone(),
                input_scope: scope,
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
            snapshot,
            owner_client_id,
            ..
        } = hello
        else {
            anyhow::bail!("terminal handshake refused");
        };
        let mut state = ScreenState {
            view: TerminalView::new(size.cols, size.rows, fg, bg)?,
            revision: 0,
            output_sequence: 0,
            owner: owner_client_id,
            control: None,
            available: false,
            exited: false,
            ack: 0,
            refused: 0,
            pending: None,
        };
        state.snapshot(snapshot, fg, bg)?;
        let screen = Arc::new(StdMutex::new(state));
        let (notify, changed) = watch::channel(0u64);
        let reader_screen = screen.clone();
        let reader = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let Ok(mut state) = reader_screen.lock() else {
                    break;
                };
                if state.apply(message, id, fg, bg).is_err() {
                    state.available = false;
                }
                notify.send_modify(|sequence| *sequence = sequence.wrapping_add(1));
            }
            if let Ok(mut state) = reader_screen.lock() {
                state.available = false;
                state.control = None;
            }
            notify.send_modify(|sequence| *sequence = sequence.wrapping_add(1));
        });
        abort.0.take();
        Ok(Self {
            id,
            connection: Uuid::new_v4(),
            entry,
            screen,
            serial: Mutex::new(()),
            incoming,
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
    pub async fn send(&self, message: ClientMsg) -> Result<()> {
        self.incoming.send(message).await.map_err(Into::into)
    }
}
