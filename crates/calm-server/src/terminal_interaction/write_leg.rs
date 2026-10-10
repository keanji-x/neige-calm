//! The one replay contract and the one write leg of `neige_terminal_input` (#2527, #2528). Typed
//! actions (`text`, `submit`, `key`, `sequence`) and `message` differ only in which client writes
//! (the caller's connection under the control it holds, or a retained kernel delivery client) and
//! in how their receipts read.
//!
//! Replay: under the caller connection's serial guard, after the caller's authorization and the
//! terminal's binding are proven and before any new-write check, a key cached on this connection
//! answers with its first receipt and writes nothing. The cache holds `unknown` from before the
//! bytes are sent, then `written` or `unknown`. A refusal proven before any byte reached the PTY is
//! not kept: the same key is decided anew, like `stale_observation` and `control_unavailable`.
use super::client::Client;
use super::target::{Resolved, refused};
use super::*;
use crate::terminal_renderer::WriteShape;
use tokio::sync::OwnedMutexGuard;

/// How long a write waits for its acknowledgement before its outcome is `unknown`.
const WRITE_BUDGET: Duration = Duration::from_secs(7);

/// A replay has no receipt to answer with once the renderer entry (and with it the connection's
/// cache) is gone.
const NO_RECEIPT: &str = "the worker's terminal has no live view (after a server restart until \
     reattached, #2499), so this connection holds no receipt to replay: this call wrote nothing, \
     and the outcome of an earlier write under this idempotency_key is unknown to the kernel. Read \
     the task and terminal state before sending again";

/// A request that may decide a new write: the binding re-proven under the serial guard.
pub(super) struct Admitted {
    pub(super) resolved: target::Resolved,
    pub(super) client: Arc<Client>,
    /// Held until the request returns: one action at a time per connection, readback included.
    pub(super) _serial: OwnedMutexGuard<()>,
}

pub(super) enum Admission {
    /// The key's first receipt, with a readback against the current screen.
    Replayed(Value),
    New(Admitted),
}

/// How the one write ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Delivered {
    /// Proven before any byte reached the PTY: the pump never received the input, or the session
    /// or the writer refused it before the write.
    Refused(String),
    Written,
    /// The bytes may have reached the PTY; no acknowledgement within [`WRITE_BUDGET`].
    Unknown,
}

impl TerminalInteraction {
    /// The one admission order of every write request: arguments, the caller's authorization and
    /// target, the serial guard, the binding re-proven, the replay, then `new_write` (the action's
    /// own write rule). With no renderer entry there is no connection and no cache: `new_write`
    /// still answers first, then the no-receipt refusal.
    pub(super) async fn admit_write(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        key: &str,
        fingerprint: &str,
        observation_wait: Option<WaitPlan>,
        new_write: impl Fn(&Resolved) -> Result<()>,
    ) -> Result<Admission> {
        if let Some(wait) = &observation_wait {
            wait.validate()?;
        }
        ensure!(
            !key.is_empty() && key.len() <= 128,
            "invalid input idempotency_key"
        );
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        if self.renderer.get(&resolved.binding.terminal_id).is_none() {
            new_write(&resolved)?;
            return Err(refused("terminal_unreadable", NO_RECEIPT.into()).into());
        }
        let client = self.client(identity, &resolved.binding).await?;
        let serial = {
            let _queued = client.queued_for_serial();
            client.serial.clone().lock_owned().await
        };
        // Decided under the serial guard: a binding that changed while queued is refused, and a
        // task that finished meanwhile still replays a key it wrote.
        let resolved =
            Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        if let Some(replayed) = self
            .replay(identity, &client, key, fingerprint, observation_wait)
            .await?
        {
            return Ok(Admission::Replayed(replayed));
        }
        new_write(&resolved)?;
        Ok(Admission::New(Admitted {
            resolved,
            client,
            _serial: serial,
        }))
    }
    /// The receipt cached under `key` on this connection, with its readback, or `None` for a new
    /// key. A key reused with other arguments is refused.
    async fn replay(
        &self,
        identity: &ToolCallIdentity,
        client: &Arc<Client>,
        key: &str,
        fingerprint: &str,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Option<Value>> {
        let cached = {
            let requests = client
                .requests
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal client poisoned"))?;
            if let Some((prior, result)) = requests.get(key) {
                ensure!(
                    prior == fingerprint,
                    "input idempotency_key reused with different arguments"
                );
                Some(result.clone())
            } else {
                ensure!(
                    requests.len() < 4096,
                    "terminal connection receipt limit reached; detach and read on a fresh connection"
                );
                None
            }
        };
        let Some(receipt) = cached else {
            return Ok(None);
        };
        // A replayed receipt's readback compares against the CURRENT state, not the state before
        // the original write.
        let current = Self::current_baseline(client);
        Ok(Some(
            self.with_observation(identity, client, receipt, observation_wait, current)
                .await,
        ))
    }
    /// The revision and signal seq right now (a replayed receipt's readback
    /// baseline); `None` when the projection is unavailable.
    pub(super) fn current_baseline(client: &Client) -> Option<ReadbackBaseline> {
        let signal_seq = client.entry.signals.last_seq();
        client
            .entry
            .handle
            .model_view
            .lock()
            .ok()
            .and_then(|view| view.capture(0).ok())
            .map(|(_, revision)| ReadbackBaseline {
                revision,
                signal_seq,
            })
    }
    /// The connection's pending-write fence, shared by `input` and `message`: no new write while
    /// an earlier one (typed input or a retained message delivery) has no known outcome.
    pub(super) fn ensure_writable(client: &Client) -> Result<()> {
        let delivery_unresolved = client.delivery_unresolved();
        let state = client
            .screen
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        ensure!(
            state.available && !state.exited && state.pending.is_none() && !delivery_unresolved,
            "terminal unavailable or prior input outcome unknown"
        );
        Ok(())
    }
}

impl TerminalInteraction {
    /// The one write leg: wait for a slot in the writer's command channel, then, with no await in
    /// between, reserve the writer's next input sequence (and retain a kernel writer on the
    /// caller's connection), cache the `unknown` receipt and enqueue the write; await its outcome
    /// within [`WRITE_BUDGET`], classify it and apply the cache rule ([`remember`]). `holder` is
    /// the caller's connection (cache and fence); `writer` is `None` when the holder writes. A
    /// request cancelled before the enqueue changed nothing; one cancelled after it leaves
    /// `unknown` cached and the fence (`pending`, or the retained delivery) up until the ack or
    /// refusal is seen.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn write_once(
        &self,
        holder: &Client,
        writer: Option<Arc<Client>>,
        key: &str,
        fingerprint: &str,
        bytes: Vec<u8>,
        shape: WriteShape,
        receipt: impl Fn(&Delivered) -> Value,
    ) -> Result<(Delivered, Value)> {
        let client = writer.as_deref().unwrap_or(holder);
        #[cfg(feature = "fixtures")]
        self.run_enqueue_seam(&client.binding.terminal_id).await;
        let Ok(slot) = client.input_slot().await else {
            // The pump is gone: no input can reach it, and nothing was reserved or cached.
            let outcome = Delivered::Refused("terminal disconnected before the write".into());
            let result = receipt(&outcome);
            return Ok((outcome, result));
        };
        let sequence = {
            let mut state = client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            let sequence = state
                .ack
                .max(state.refused)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("input sequence exhausted"))?;
            state.pending = Some(sequence);
            sequence
        };
        if let Some(writer) = &writer {
            *holder
                .delivery
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((writer.clone(), sequence));
        }
        cache(holder, key, fingerprint, &receipt(&Delivered::Unknown));
        slot.send(bytes, sequence, shape);
        let settled = client
            .wait(
                |state| state.ack >= sequence || state.refused >= sequence,
                WRITE_BUDGET,
            )
            .await;
        let outcome = match (settled, client.screen.lock()) {
            (Ok(()), Ok(state)) if state.ack >= sequence => Delivered::Written,
            (Ok(()), Ok(state)) => Delivered::Refused(
                state
                    .last_protocol_error
                    .clone()
                    .unwrap_or_else(|| "terminal input refused".into()),
            ),
            _ => Delivered::Unknown,
        };
        if writer.is_some() && outcome != Delivered::Unknown {
            holder.delivery_release();
        }
        let result = receipt(&outcome);
        remember(holder, key, fingerprint, &outcome, &result);
        Ok((outcome, result))
    }
    /// Test seam: runs in [`Self::write_once`] before it waits for the writer's channel slot,
    /// given the terminal id, so a test can cancel a request before its write is enqueued.
    /// Consumed once.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn set_enqueue_seam(&self, seam: ClaimWindowSeam) {
        *self.enqueue_seam.lock().unwrap_or_else(|e| e.into_inner()) = Some(seam);
    }
    #[cfg(feature = "fixtures")]
    async fn run_enqueue_seam(&self, terminal_id: &str) {
        let seam = self
            .enqueue_seam
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(seam) = seam {
            seam(terminal_id.to_owned()).await;
        }
    }
}

/// The cache rule: a `written` or `unknown` receipt stays under its key; a proven refusal wrote
/// nothing, so its key is forgotten and a resend is decided anew.
pub(super) fn remember(
    holder: &Client,
    key: &str,
    fingerprint: &str,
    outcome: &Delivered,
    receipt: &Value,
) {
    match outcome {
        Delivered::Refused(_) => {
            holder
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key);
        }
        Delivered::Written | Delivered::Unknown => cache(holder, key, fingerprint, receipt),
    }
}

fn cache(client: &Client, key: &str, fingerprint: &str, receipt: &Value) {
    client
        .requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.to_owned(), (fingerprint.to_owned(), receipt.clone()));
}
