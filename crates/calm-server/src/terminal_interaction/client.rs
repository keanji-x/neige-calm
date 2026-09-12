use crate::terminal_renderer::{
    ClaimOutcome, ClientInputScope, ClientPumpContext, INPUT_REVOKED_BEFORE_WRITE, PumpCommand,
    RendererEntry, run_client_pump_with_commands,
};
use anyhow::{Result, ensure};
use calm_session::terminal_session::INPUT_REQUIRES_OWNER_ROLE;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION,
    ProtocolErrorCode, PtySize, RenderEncoding, Role,
};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
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
    /// `OwnerChanged` deliveries that named this connection (control was
    /// granted). A granted claim-if-unowned waits on this counter, not on
    /// `owner == me`: a grant and a takeover applied back to back leave
    /// `owner` naming the other client, and the claim must still read the
    /// takeover instead of idling to its budget (#1620 R6).
    pub grants: u64,
}
/// Whether a protocol error is the refusal of a pending input, so the input's
/// fate is known and `pending` may become `refused`. Both input refusals and
/// ownership-claim refusals use `NotOwner` and the wire carries no input seq,
/// so the two input messages are matched exactly; every other error (a
/// refused claim-if-unowned, a revoked claim scope, a lease exhaustion) leaves
/// a pending input UNKNOWN and later writes stay fenced (#1620 R5).
pub fn refers_to_pending_input(code: ProtocolErrorCode, message: &str) -> bool {
    code == ProtocolErrorCode::NotOwner
        && (message == INPUT_REQUIRES_OWNER_ROLE || message == INPUT_REVOKED_BEFORE_WRITE)
}
impl ScreenState {
    fn apply(&mut self, message: DaemonMsg, id: Uuid) -> Result<()> {
        match message {
            DaemonMsg::OwnerChanged { owner_client_id } => {
                self.owner = owner_client_id;
                self.control = if self.owner == Some(id) {
                    self.grants = self.grants.wrapping_add(1);
                    Some(Uuid::new_v4())
                } else {
                    None
                };
            }
            DaemonMsg::InputAck { input_seq } => {
                self.ack = input_seq;
                self.pending = None;
            }
            DaemonMsg::ProtocolError { code, message, .. } => {
                if refers_to_pending_input(code, &message)
                    && let Some(pending) = self.pending.take()
                {
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
    /// Test seam (#1620): the reader takes this lock before applying each
    /// daemon message, so a test can hold protocol delivery (an
    /// `OwnerChanged`, a refusal) on this connection while the registry moves.
    #[cfg(feature = "fixtures")]
    pub delivery_gate: Arc<Mutex<()>>,
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
            grants: 0,
        };
        let screen = Arc::new(StdMutex::new(state));
        #[cfg(feature = "fixtures")]
        let delivery_gate = Arc::new(Mutex::new(()));
        #[cfg(feature = "fixtures")]
        let reader_gate = delivery_gate.clone();
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
                        #[cfg(feature = "fixtures")]
                        let _delivery = reader_gate.lock().await;
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
            #[cfg(feature = "fixtures")]
            delivery_gate,
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
    /// it (decided under the owner-registry lock). The pump's own verdict
    /// arrives on the returned channel (R6); a grant is additionally
    /// delivered as an `OwnerChanged` naming this client (`grants`).
    pub async fn claim_if_unowned(&self) -> Result<oneshot::Receiver<ClaimOutcome>> {
        let (reply, outcome) = oneshot::channel();
        self.commands
            .send(PumpCommand::ClaimIfUnowned { reply })
            .await?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_renderer::CONTROL_HELD_BY_ANOTHER_CLIENT;

    fn state(pending: Option<u64>) -> ScreenState {
        ScreenState {
            owner: Some(Uuid::new_v4()),
            control: Some(Uuid::new_v4()),
            available: true,
            exited: false,
            ack: 2,
            refused: 0,
            pending,
            protocol_errors: 0,
            last_protocol_error: None,
            grants: 0,
        }
    }
    fn error(code: ProtocolErrorCode, message: &str) -> DaemonMsg {
        DaemonMsg::ProtocolError {
            code,
            message: message.to_owned(),
            expected_version: None,
        }
    }

    /// #1620 R5 — an ownership error never resolves an UNKNOWN input: the
    /// reservation stays, so the next input is still fenced by
    /// `state.pending.is_none()` ("prior input outcome unknown"). The
    /// integration harness cannot leave `pending` set (its supervisor
    /// acknowledges or refuses every write), so the classification is
    /// pinned here on the state machine itself.
    #[test]
    fn claim_refusal_leaves_a_pending_input_unknown() {
        let me = Uuid::new_v4();
        for message in [
            CONTROL_HELD_BY_ANOTHER_CLIENT,
            "terminal control is unavailable or its scope was revoked",
        ] {
            let mut state = state(Some(3));
            state
                .apply(error(ProtocolErrorCode::NotOwner, message), me)
                .unwrap();
            assert_eq!(state.pending, Some(3), "{message}");
            assert_eq!(state.refused, 0, "{message}");
            assert_eq!(state.protocol_errors, 1);
            assert_eq!(state.last_protocol_error.as_deref(), Some(message));
        }
        let mut state = state(Some(3));
        state
            .apply(
                error(
                    ProtocolErrorCode::BadSequence,
                    "terminal owner lease generation exhausted",
                ),
                me,
            )
            .unwrap();
        assert_eq!(state.pending, Some(3));
        assert_eq!(state.refused, 0);
    }

    /// The two input refusals do resolve the reservation as refused.
    #[test]
    fn input_refusals_resolve_the_pending_input() {
        let me = Uuid::new_v4();
        for message in [INPUT_REQUIRES_OWNER_ROLE, INPUT_REVOKED_BEFORE_WRITE] {
            let mut state = state(Some(3));
            state
                .apply(error(ProtocolErrorCode::NotOwner, message), me)
                .unwrap();
            assert_eq!(state.pending, None, "{message}");
            assert_eq!(state.refused, 3, "{message}");
        }
        // The message alone is not enough: the code must be NotOwner.
        let mut state = state(Some(3));
        state
            .apply(
                error(ProtocolErrorCode::BadSequence, INPUT_REQUIRES_OWNER_ROLE),
                me,
            )
            .unwrap();
        assert_eq!(state.pending, Some(3));
    }

    /// #1620 R6 — a grant is counted even when folded with a later takeover,
    /// which leaves `owner` naming the other client.
    #[test]
    fn grants_count_a_folded_grant() {
        let me = Uuid::new_v4();
        let human = Uuid::new_v4();
        let mut state = state(None);
        state.owner = None;
        state.control = None;
        state
            .apply(
                DaemonMsg::OwnerChanged {
                    owner_client_id: Some(me),
                },
                me,
            )
            .unwrap();
        assert!(state.control.is_some());
        state
            .apply(
                DaemonMsg::OwnerChanged {
                    owner_client_id: Some(human),
                },
                me,
            )
            .unwrap();
        assert_eq!(state.owner, Some(human));
        assert_eq!(state.control, None);
        assert_eq!(state.grants, 1, "the folded grant is still counted");
    }
}
