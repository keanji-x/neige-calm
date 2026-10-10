use super::actions::{Encoded, encode, sequence_steps};
use super::input_control::ClaimStep;
use super::receipts::{
    WriteReceipts, attach_claim, control_unavailable_receipt, merge, stale_receipt,
};
use super::screen_diff::{CursorSnapshot, ScreenDiff, row_hashes};
use super::write_leg::{Admission, remember, write_once};
use super::*;
use crate::terminal_renderer::WriteShape;

/// Per-request switches of an input. All three enter the request fingerprint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputOptions {
    /// Replace the exact-revision fence with a same-surface fence.
    pub allow_output_since_observation: bool,
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
    /// under the serial guard is fixed: binding → replay → write rule → fences → claim → pre-write
    /// capture → write → release → readback ([`Self::admit_write`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn input(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        observation: Option<Uuid>,
        idempotency_key: &str,
        action: Value,
        options: InputOptions,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Value> {
        let key = idempotency_key.to_owned();
        // The fingerprint hashes the arguments as given (null when omitted) so a replayed
        // idempotency_key returns the same receipt and never claims, releases or writes again.
        let fingerprint = crate::routes::idempotency_key::stable_payload_hash(&json!({
            "observation_id":observation,"action":action,
            "allow_output_since_observation":options.allow_output_since_observation,
            "claim":options.claim,"release":options.release
        }))?;
        // The write rule is decided under the serial guard, after the replay: a task that finished
        // while this request queued is refused (never answered stale_observation), and a key it
        // wrote still replays.
        let admitted = match self
            .admit_write(
                identity,
                target,
                &key,
                &fingerprint,
                observation_wait.clone(),
                |resolved| self.ensure_keys_accepted(resolved),
            )
            .await?
        {
            Admission::Replayed(replayed) => return Ok(replayed),
            Admission::New(admitted) => admitted,
        };
        let (resolved, client) = (&admitted.resolved, &admitted.client);
        let terminal = resolved.binding.terminal_id.as_str();
        let observation = match observation {
            Some(id) => id,
            None => client
                .latest_observation
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal client poisoned"))?
                .map(|latest| latest.id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no observation on this connection; read first with neige_terminal_read"
                    )
                })?,
        };
        let saved = self.saved_observation(identity, resolved, client, observation)?;
        Self::ensure_writable(client)?;
        // The checks that need no live screen come first, so a claim is never granted on a
        // request that errors anyway.
        ensure!(
            saved.surface.scroll_offset == 0,
            "return to live viewport before input"
        );
        encode(&action, &saved.surface)?;
        // 2. Claim: decided before the pre-write capture, since a granted claim changes what the control fence compares.
        let claim = match options.claim {
            true => Some(self.claim_for_input(client, saved.control).await?),
            false => None,
        };
        if let Some(ClaimStep::Unavailable { status, reason }) = &claim {
            // No write and nothing cached: a resend after the human is done must not conflict.
            let receipt =
                control_unavailable_receipt(terminal, idempotency_key, observation, status, reason);
            return Ok(self
                .with_observation(identity, client, receipt, Some(WaitPlan::default()), None)
                .await);
        }
        // 3. Pre-write capture and the remaining fences. An RPC error carries no receipt: every
        // error from here on says that the caller now holds the control it claimed.
        let fence = self
            .pre_write_fences(client, &saved, &action, options, claim.as_ref())
            .map_err(|error| note_claim(error, claim.as_ref()))?;
        let Ready {
            bytes,
            shape,
            input_revision,
            signal_seq,
            tolerated,
        } = match fence {
            Fence::Ready(ready) => ready,
            Fence::ControlLost => {
                // Granted, then taken over before the fence read the lease: fail closed.
                let receipt = control_unavailable_receipt(
                    terminal,
                    idempotency_key,
                    observation,
                    "unavailable",
                    CONTROL_TAKEN_BY_ANOTHER_CLIENT,
                );
                return Ok(self
                    .with_observation(identity, client, receipt, Some(WaitPlan::default()), None)
                    .await);
            }
            Fence::Stale { current, diff } => {
                // No physical write and nothing cached under the idempotency_key: a later resend must not conflict.
                let mut receipt = stale_receipt(
                    terminal,
                    idempotency_key,
                    observation,
                    saved.revision,
                    current,
                    &diff,
                );
                attach_claim(&mut receipt, claim.as_ref());
                return Ok(self
                    .with_observation(identity, client, receipt, Some(WaitPlan::default()), None)
                    .await);
            }
        };
        let drift = (input_revision != saved.revision).then(|| {
            let mut drift =
                json!({"observed_revision":saved.revision,"input_revision":input_revision});
            if let Some(diff) = &tolerated {
                merge(&mut drift, diff.drift_json());
            }
            drift
        });
        let mut receipts = WriteReceipts::new(
            terminal,
            idempotency_key,
            observation,
            drift.as_ref(),
            sequence_steps(&action),
            options.release,
        );
        receipts.attach(claim.as_ref());
        let (outcome, mut result) =
            write_once(client, None, &key, &fingerprint, bytes, shape, |outcome| {
                receipts.for_outcome(outcome)
            })
            .await?;
        // 5. Release: after the write's outcome is known and cached; never clears `pending`. A call
        // cancelled here leaves `requested` in the cached receipt and a replay never releases.
        if options.release {
            result["release"] = self.release(client).await.to_json();
            remember(client, &key, &fingerprint, &outcome, &result).await;
        }
        Ok(self
            .with_observation(
                identity,
                client,
                result,
                observation_wait,
                Some(ReadbackBaseline {
                    revision: input_revision,
                    signal_seq,
                }),
            )
            .await)
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
            .ok_or_else(|| anyhow::anyhow!("observation expired; read again"))?;
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
    /// The fences that read the live screen: availability and age again (the claim may have
    /// taken seconds), control, surface, action, revision.
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
                anyhow::bail!("terminal control changed; read before input")
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
            "terminal surface changed since observation (size, input modes or alternate screen); read again"
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
            if !options.allow_output_since_observation {
                return Ok(Fence::Stale { current, diff });
            }
            Some(diff)
        };
        let (bytes, shape) = match encoded {
            Encoded::Bytes(bytes) => (bytes, WriteShape::Verbatim),
            Encoded::Submit(bytes) => (bytes, WriteShape::SplitTrailingCr),
        };
        Ok(Fence::Ready(Ready {
            bytes,
            shape,
            input_revision: current,
            signal_seq,
            tolerated,
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
    /// A moved revision admitted by `allow_output_since_observation`: the row comparison.
    tolerated: Option<ScreenDiff>,
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
