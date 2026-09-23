use super::actions::{BELOW_CURSOR_EDITS_ONLY, Encoded, edits_the_draft, encode, sequence_steps};
use super::input_control::ClaimStep;
use super::receipts::{
    WriteReceipts, attach_claim, control_unavailable_receipt, merge, stale_receipt,
};
use super::replace_plan::ReplacePlan;
use super::screen_diff::{CursorSnapshot, ScreenDiff, Tolerance, row_hashes};
use super::*;
use crate::terminal_renderer::WriteShape;

/// Per-request switches of an input. All four enter the request fingerprint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputOptions {
    /// Replace the exact-revision fence with a same-surface fence.
    pub allow_output_since_observation: bool,
    /// Admit a moved revision when only rows strictly below an unmoved,
    /// visible cursor changed (never for clicks).
    pub allow_output_below_cursor: bool,
    /// Claim control (if unowned) before the write when the observation was
    /// taken as observer and this connection holds no control.
    pub claim: bool,
    /// Release control after the write, before the readback.
    pub release: bool,
}

/// The observation's facts, copied out of the registry so no lock is held across the claim.
struct Saved {
    revision: u64,
    control: Option<Uuid>,
    surface: InputSurface,
    cursor: CursorSnapshot,
    row_hashes: Vec<u64>,
    created: Instant,
}
/// Checked when the input names the observation and again at the pre-write fences, since
/// the claim in between can take up to 14 s.
const OBSERVATION_TTL: Duration = Duration::from_secs(120);
fn fresh(created: Instant) -> bool {
    created.elapsed() < OBSERVATION_TTL
}
const OBSERVATION_EXPIRED: &str = "observation belongs to another connection or expired";

impl TerminalInteraction {
    /// `observation` is the caller's argument; `None` selects this connection's latest. The order
    /// under the serial guard is fixed: fences → claim → pre-write capture → write → release → readback.
    #[allow(clippy::too_many_arguments)]
    pub async fn input(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        observation: Option<Uuid>,
        request_key: &str,
        action: Value,
        options: InputOptions,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Value> {
        if let Some(wait) = &observation_wait {
            wait.validate()?;
        }
        ensure!(
            !request_key.is_empty() && request_key.len() <= 128,
            "invalid input request key"
        );
        ensure!(
            !options.allow_output_below_cursor || edits_the_draft(&action),
            "{BELOW_CURSOR_EDITS_ONLY}"
        );
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        resolved.ensure_accepts_input()?;
        let terminal = resolved.binding.terminal_id.as_str();
        let client = self.client(identity, &resolved.binding).await?;
        // One action at a time per connection, readback wait included; other connections are not serialized.
        let _serial = {
            let _queued = client.queued_for_serial();
            client.serial.lock().await
        };
        // Write authority is decided under the serial lock: a task that finished during the queue
        // must not be answered with stale_observation although write authority is gone.
        Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, true).await?;
        let key = request_key.to_owned();
        // The fingerprint hashes the arguments as given (null when omitted) so a replayed
        // request_id returns the same receipt and never claims, releases or writes again.
        let fingerprint = crate::routes::terminal_cards::stable_payload_hash(&json!({
            "observation_id":observation,"action":action,
            "allow_output_since_observation":options.allow_output_since_observation,
            "allow_output_below_cursor":options.allow_output_below_cursor,
            "claim":options.claim,"release":options.release
        }))?;
        let cached = {
            let requests = client.requests.lock().await;
            if let Some((prior, result)) = requests.get(&key) {
                ensure!(
                    prior == &fingerprint,
                    "input request key reused with different arguments"
                );
                Some(result.clone())
            } else {
                ensure!(
                    requests.len() < 4096,
                    "terminal connection receipt limit reached; detach and observe a fresh connection"
                );
                None
            }
        };
        if let Some(receipt) = cached {
            // A replayed receipt's readback compares against the CURRENT state, not the state before
            // the original write.
            let current = Self::current_baseline(&client);
            return Ok(self
                .with_observation(identity, &client, receipt, observation_wait, current)
                .await);
        }
        let observation = match observation {
            Some(id) => id,
            None => client
                .latest_observation
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal client poisoned"))?
                .map(|latest| latest.id)
                .ok_or_else(|| {
                    anyhow::anyhow!("no observation on this connection; observe first")
                })?,
        };
        let saved = self.saved_observation(identity, &resolved, &client, observation)?;
        Self::ensure_writable(&client)?;
        // The checks that need no live screen come first, so a claim is never granted on a
        // request that errors anyway.
        ensure!(
            saved.surface.scroll_offset == 0,
            "return to live viewport before input"
        );
        encode(&action, &saved.surface)?;
        // 2. Claim: decided before the pre-write capture, since a granted claim changes what the control fence compares.
        let claim = match options.claim {
            true => Some(self.claim_for_input(&client, saved.control).await?),
            false => None,
        };
        if let Some(ClaimStep::Unavailable { status, reason }) = &claim {
            // No write and nothing cached: a resend after the human is done must not conflict.
            let receipt =
                control_unavailable_receipt(terminal, request_key, observation, status, reason);
            return Ok(self
                .with_observation(identity, &client, receipt, Some(WaitPlan::default()), None)
                .await);
        }
        // 3. Pre-write capture and the remaining fences. An RPC error carries no receipt: every
        // error from here on says that the caller now holds the control it claimed.
        let fence = self
            .pre_write_fences(&client, &saved, &action, options, claim.as_ref())
            .map_err(|error| note_claim(error, claim.as_ref()))?;
        let Ready {
            bytes,
            shape,
            input_revision,
            signal_seq,
            tolerated,
            replace,
        } = match fence {
            Fence::Ready(ready) => ready,
            Fence::ControlLost => {
                // Granted, then taken over before the fence read the lease: fail closed.
                let receipt = control_unavailable_receipt(
                    terminal,
                    request_key,
                    observation,
                    "unavailable",
                    CONTROL_TAKEN_BY_ANOTHER_CLIENT,
                );
                return Ok(self
                    .with_observation(identity, &client, receipt, Some(WaitPlan::default()), None)
                    .await);
            }
            Fence::Stale { current, diff } => {
                // No physical write and nothing cached under the request_id: a later resend must not conflict.
                let mut receipt = stale_receipt(
                    terminal,
                    request_key,
                    observation,
                    saved.revision,
                    current,
                    &diff,
                );
                attach_claim(&mut receipt, claim.as_ref());
                return Ok(self
                    .with_observation(identity, &client, receipt, Some(WaitPlan::default()), None)
                    .await);
            }
        };
        let drift = (input_revision != saved.revision).then(|| {
            let mut drift =
                json!({"observed_revision":saved.revision,"input_revision":input_revision});
            if let Some((tolerance, diff)) = &tolerated {
                merge(&mut drift, diff.tolerance_json(*tolerance));
            }
            drift
        });
        let mut receipts = WriteReceipts::new(
            terminal,
            request_key,
            observation,
            drift.as_ref(),
            sequence_steps(&action),
            replace.as_ref(),
            options.release,
        );
        receipts.attach(claim.as_ref());
        let mut result = write_action(
            &client,
            key.clone(),
            fingerprint.clone(),
            bytes,
            shape,
            receipts,
        )
        .await?;
        // 5. Release: after the write's outcome is known and cached; never clears `pending`. A call
        // cancelled here leaves `requested` in the cached receipt and a replay never releases.
        if options.release {
            result["release"] = self.release(&client).await.to_json();
            cache(&client, &key, &fingerprint, &result).await;
        }
        Ok(self
            .with_observation(
                identity,
                &client,
                result,
                observation_wait,
                Some(ReadbackBaseline {
                    revision: input_revision,
                    signal_seq,
                }),
            )
            .await)
    }
    /// The revision and signal seq right now (a replayed receipt's readback
    /// baseline); `None` when the projection is unavailable.
    fn current_baseline(client: &Client) -> Option<ReadbackBaseline> {
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
    fn saved_observation(
        &self,
        identity: &ToolCallIdentity,
        resolved: &target::Resolved,
        client: &Client,
        observation: Uuid,
    ) -> Result<Saved> {
        let observations = self
            .observations
            .lock()
            .map_err(|_| anyhow::anyhow!("observation registry poisoned"))?;
        let saved = observations
            .get(&observation)
            .ok_or_else(|| anyhow::anyhow!("observation expired; observe again"))?;
        ensure!(
            saved.binding == resolved.binding.key(identity)
                && saved.connection == client.connection
                && fresh(saved.created),
            "{OBSERVATION_EXPIRED}"
        );
        Ok(Saved {
            revision: saved.revision,
            control: saved.control,
            surface: saved.surface,
            cursor: saved.cursor,
            row_hashes: saved.row_hashes.clone(),
            created: saved.created,
        })
    }
    fn ensure_writable(client: &Client) -> Result<()> {
        let state = client
            .screen
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        ensure!(
            state.available && !state.exited && state.pending.is_none(),
            "terminal unavailable or prior input outcome unknown"
        );
        Ok(())
    }
    /// The fences that read the live screen: availability and age again (the claim may have
    /// taken seconds), control, surface, action, revision; then a `replace` plan.
    fn pre_write_fences(
        &self,
        client: &Client,
        saved: &Saved,
        action: &Value,
        options: InputOptions,
        claim: Option<&ClaimStep>,
    ) -> Result<Fence> {
        Self::ensure_writable(client)?;
        ensure!(fresh(saved.created), "{OBSERVATION_EXPIRED}");
        let control = client
            .screen
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?
            .control;
        match control_fence(claim, saved.control, control) {
            ControlVerdict::Ok => {}
            ControlVerdict::Lost => return Ok(Fence::ControlLost),
            ControlVerdict::Changed => {
                anyhow::bail!("terminal control changed; observe before input")
            }
        }
        // Read immediately before the physical write: the readback baseline and the drift evidence.
        let signal_seq = client.entry.signals.last_seq();
        let (frame, current) = client
            .entry
            .handle
            .model_view
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
            .capture(0)?;
        let now = frame.input_surface();
        ensure!(
            same_input_surface(&saved.surface, &now),
            "terminal surface changed since observation (size, input modes or alternate screen); observe again"
        );
        // Encode before deciding stale vs ready: an invalid action is an RPC error whatever the
        // revision did, so only the exact-revision fence is relaxed by the stale result.
        let encoded = encode(action, &now)?;
        let tolerated = if saved.revision == current {
            None
        } else {
            // Every other fence passed and only the exact revision differs. The row comparison is
            // reported whatever admits the write; a stale result carries it too.
            let diff = ScreenDiff::compare(
                saved.cursor,
                &saved.row_hashes,
                CursorSnapshot::from(&frame.cursor),
                &row_hashes(&frame),
            );
            if options.allow_output_since_observation {
                Some((Tolerance::OutputSinceObservation, diff))
            } else if options.allow_output_below_cursor && diff.only_below_cursor() {
                Some((Tolerance::BelowCursor, diff))
            } else {
                return Ok(Fence::Stale { current, diff });
            }
        };
        // A replace looks the draft up on the live frame only once the revision (or a tolerance)
        // admitted the write; its refusals are RPC errors like an invalid action's.
        let (bytes, shape, replace) = match encoded {
            Encoded::Bytes(bytes) => (bytes, WriteShape::Verbatim, None),
            Encoded::Submit(bytes) => (bytes, WriteShape::SplitTrailingCr, None),
            Encoded::Replace { from, to } => {
                let plan = ReplacePlan::derive(&frame, &from, &to)?;
                (plan.bytes(&now)?, WriteShape::Verbatim, Some(plan))
            }
        };
        Ok(Fence::Ready(Ready {
            bytes,
            shape,
            input_revision: current,
            signal_seq,
            tolerated,
            replace,
        }))
    }
}
/// Outcome of the pre-write fences: a stale observation (only the exact-revision fence failed)
/// becomes a structured refusal rather than an error.
enum Fence {
    Ready(Ready),
    Stale {
        current: u64,
        diff: ScreenDiff,
    },
    /// The lease a claim granted is no longer this connection's.
    ControlLost,
}
/// The control fence. A granted claim authorizes the observer → owner transition only for the
/// lease it granted: a takeover applied since is `Lost` and fails closed.
#[derive(Debug, PartialEq, Eq)]
enum ControlVerdict {
    Ok,
    Lost,
    Changed,
}
fn control_fence(
    claim: Option<&ClaimStep>,
    saved: Option<Uuid>,
    live: Option<Uuid>,
) -> ControlVerdict {
    match claim {
        Some(ClaimStep::Claimed(control)) if live == Some(*control) => ControlVerdict::Ok,
        Some(ClaimStep::Claimed(_)) => ControlVerdict::Lost,
        _ if saved.is_some() && saved == live => ControlVerdict::Ok,
        _ => ControlVerdict::Changed,
    }
}
/// An RPC error carries no receipt: after a granted claim it says so.
fn note_claim(error: anyhow::Error, claim: Option<&ClaimStep>) -> anyhow::Error {
    match claim {
        Some(ClaimStep::Claimed(control)) => {
            anyhow::anyhow!("{error}; control claimed (control_id {control})")
        }
        _ => error,
    }
}
struct Ready {
    bytes: Vec<u8>,
    /// How the writer hands `bytes` to the PTY (`submit` splits).
    shape: WriteShape,
    input_revision: u64,
    signal_seq: u64,
    /// A moved revision: the opt-in that admitted it and the row comparison.
    tolerated: Option<(Tolerance, ScreenDiff)>,
    replace: Option<ReplacePlan>,
}
/// Reserve the next input sequence, cache the unknown receipt, send one ordered write and await
/// its ack. Cancellation preserves Unknown and blocks all subsequent writes until the matching ack/refusal is observed.
async fn write_action(
    client: &Client,
    key: String,
    fingerprint: String,
    bytes: Vec<u8>,
    shape: WriteShape,
    receipts: WriteReceipts,
) -> Result<Value> {
    let sequence = {
        let mut state = client.screen.lock().unwrap();
        let sequence = state
            .ack
            .max(state.refused)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("input sequence exhausted"))?;
        state.pending = Some(sequence);
        sequence
    };
    cache(client, &key, &fingerprint, &receipts.unknown).await;
    let result = if client.send_input(bytes, sequence, shape).await.is_err() {
        receipts.unknown
    } else {
        match client
            .wait(
                |state| state.ack >= sequence || state.refused >= sequence,
                Duration::from_secs(7),
            )
            .await
        {
            Ok(()) => {
                if client.screen.lock().unwrap().ack >= sequence {
                    receipts.written
                } else {
                    receipts.refused
                }
            }
            Err(_) => receipts.unknown,
        }
    };
    cache(client, &key, &fingerprint, &result).await;
    Ok(result)
}
async fn cache(client: &Client, key: &str, fingerprint: &str, receipt: &Value) {
    client
        .requests
        .lock()
        .await
        .insert(key.to_owned(), (fingerprint.to_owned(), receipt.clone()));
}

#[cfg(test)]
mod fence_tests {
    use super::*;

    #[test]
    fn control_fence_authorizes_only_the_granted_lease() {
        let (mine, other, observed) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let claimed = ClaimStep::Claimed(mine);
        assert_eq!(
            control_fence(Some(&claimed), None, Some(mine)),
            ControlVerdict::Ok
        );
        assert_eq!(
            control_fence(Some(&claimed), None, Some(other)),
            ControlVerdict::Lost,
            "taken over between the post-grant re-read and the fence"
        );
        assert_eq!(
            control_fence(Some(&claimed), None, None),
            ControlVerdict::Lost
        );
        for claim in [None, Some(&ClaimStep::Held)] {
            assert_eq!(
                control_fence(claim, Some(observed), Some(observed)),
                ControlVerdict::Ok
            );
            assert_eq!(
                control_fence(claim, Some(observed), Some(other)),
                ControlVerdict::Changed
            );
            assert_eq!(
                control_fence(claim, None, Some(other)),
                ControlVerdict::Changed,
                "an observer observation without a claim"
            );
            assert_eq!(control_fence(claim, None, None), ControlVerdict::Changed);
            assert_eq!(
                control_fence(claim, Some(observed), None),
                ControlVerdict::Changed
            );
        }
        let noted = note_claim(anyhow::anyhow!("boom"), Some(&claimed));
        assert_eq!(
            noted.to_string(),
            format!("boom; control claimed (control_id {mine})")
        );
        assert_eq!(
            note_claim(anyhow::anyhow!("boom"), Some(&ClaimStep::Held)).to_string(),
            "boom"
        );
        assert_eq!(
            note_claim(anyhow::anyhow!("boom"), None).to_string(),
            "boom"
        );
        assert!(fresh(Instant::now()));
        assert!(fresh(
            Instant::now() - (OBSERVATION_TTL - Duration::from_secs(1))
        ));
        assert!(!fresh(Instant::now() - OBSERVATION_TTL));
    }
}
