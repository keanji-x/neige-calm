//! Control per scenario (#1666): `input claim:true` folds a claim-if-unowned
//! into the first observed input of a scenario, `release:true` gives control
//! back right after the last write. Both helpers assume the caller holds the
//! connection's serial guard (the public `control()` re-takes it and must
//! not be called from here).
use super::*;

/// The claim step of an `input claim:true`, decided before the pre-write
/// capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClaimStep {
    /// This connection already holds control: no claim was sent and the
    /// ordinary control fence applies unchanged.
    Held,
    /// A claim-if-unowned was granted and its `OwnerChanged` applied: the
    /// None → Some transition is authorized for this input, and the new
    /// lease is the control id.
    Claimed(Uuid),
    /// No write may follow. `status` is `unavailable` (another client owns
    /// the terminal, a takeover folded with the grant, or the observation's
    /// control is no longer held) or `unconfirmed` (the claim or its
    /// delivery timed out: a later `claim:true` on this connection reports
    /// what stands).
    Unavailable {
        status: &'static str,
        reason: String,
    },
}
impl ClaimStep {
    pub(super) fn to_json(&self) -> Value {
        match self {
            Self::Held => json!({"status":"held"}),
            Self::Claimed(control) => json!({"status":"claimed","control_id":control}),
            Self::Unavailable { status, reason } => json!({"status":status,"reason":reason}),
        }
    }
}

/// Reason of an `input claim:true` whose observation was taken as owner
/// while this connection no longer holds control.
pub const CONTROL_NO_LONGER_HELD: &str =
    "terminal control held at the observation is no longer held; observe before input";

impl TerminalInteraction {
    /// The claim step (#1666 S3). Held control needs no claim. An observer
    /// observation (`saved_control == None`) on a connection without control
    /// runs the same atomic claim-if-unowned as `open claim:true` (the pump
    /// decides under the owner-registry lock and never displaces a human),
    /// takes the pump's own verdict, waits for the grant to be applied and
    /// re-reads what stands: a grant folded with a takeover never shows
    /// `owner == me` and is a refusal, as `open` detects. Anything else
    /// (`saved_control` set but not held any more) is a refusal without a
    /// claim: the observation described a lease this connection lost.
    pub(super) async fn claim_for_input(
        &self,
        client: &Client,
        saved_control: Option<Uuid>,
    ) -> Result<ClaimStep> {
        let (control, grants_before) = {
            let state = client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            (state.control, state.grants)
        };
        if control.is_some() {
            return Ok(ClaimStep::Held);
        }
        if saved_control.is_some() {
            return Ok(ClaimStep::Unavailable {
                status: "unavailable",
                reason: CONTROL_NO_LONGER_HELD.into(),
            });
        }
        #[cfg(feature = "fixtures")]
        self.run_claim_window_seam(&client.binding.terminal_id)
            .await;
        let budget = Duration::from_secs(7);
        let started = Instant::now();
        let unconfirmed = |reason: &str| ClaimStep::Unavailable {
            status: "unconfirmed",
            reason: reason.to_owned(),
        };
        let outcome = match tokio::time::timeout(budget, client.claim_if_unowned().await?).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => anyhow::bail!("terminal disconnected"),
            Err(_) => return Ok(unconfirmed("terminal claim timed out")),
        };
        if let ClaimOutcome::Refused { reason } = outcome {
            return Ok(ClaimStep::Unavailable {
                status: "unavailable",
                reason,
            });
        }
        // Granted: the `OwnerChanged` naming this connection mints the control
        // id when applied (counted even when folded with a takeover).
        match client
            .wait(
                |state| state.grants != grants_before,
                budget.saturating_sub(started.elapsed()),
            )
            .await
        {
            Ok(()) => {}
            Err(error)
                if error
                    .downcast_ref::<tokio::time::error::Elapsed>()
                    .is_some() =>
            {
                return Ok(unconfirmed(
                    "terminal claim granted but its delivery timed out",
                ));
            }
            Err(error) => return Err(error),
        }
        let state = client
            .screen
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
        match state.control {
            Some(control) if state.owner == Some(client.id) => Ok(ClaimStep::Claimed(control)),
            _ => Ok(ClaimStep::Unavailable {
                status: "unavailable",
                reason: CONTROL_TAKEN_BY_ANOTHER_CLIENT.into(),
            }),
        }
    }
    /// The release step after the write (#1666 S3): `released` when this
    /// connection held control (cache and registry agree) and the release
    /// was applied, `not_held` when it did not (a takeover, or a lease the
    /// cache still shows while the registry names another client),
    /// `unconfirmed` when the release could not be sent or its delivery
    /// timed out. Never touches `pending` or the write outcome.
    pub(super) async fn release_after_input(&self, client: &Client) -> &'static str {
        let held = client
            .screen
            .lock()
            .is_ok_and(|state| state.control.is_some())
            && client
                .entry
                .handle
                .owner_registry
                .lock()
                .is_ok_and(|registry| registry.current_owner() == Some(client.id));
        if !held {
            return "not_held";
        }
        if client.send(ClientMsg::OwnerRelease).await.is_err() {
            return "unconfirmed";
        }
        match client
            .wait(|state| state.control.is_none(), Duration::from_secs(7))
            .await
        {
            Ok(()) => "released",
            Err(_) => "unconfirmed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_step_json_shapes() {
        let control = Uuid::new_v4();
        assert_eq!(ClaimStep::Held.to_json(), json!({"status":"held"}));
        assert_eq!(
            ClaimStep::Claimed(control).to_json(),
            json!({"status":"claimed","control_id":control})
        );
        assert_eq!(
            ClaimStep::Unavailable {
                status: "unconfirmed",
                reason: "terminal claim timed out".into()
            }
            .to_json(),
            json!({"status":"unconfirmed","reason":"terminal claim timed out"})
        );
    }
}
