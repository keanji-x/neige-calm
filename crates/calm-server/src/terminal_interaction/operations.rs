use super::actions::{encode, sequence_steps};
use super::input_control::ClaimStep;
use super::screen_diff::{CursorSnapshot, ScreenDiff, row_hashes};
use super::*;

/// Per-request switches of an input: the #1618 drift opt-in, and the #1666
/// below-cursor tolerance and control steps. All four enter the request
/// fingerprint.
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

/// The facts of the observation an input names, copied out of the registry
/// so no lock is held across the claim.
struct Saved {
    revision: u64,
    control: Option<Uuid>,
    surface: InputSurface,
    cursor: CursorSnapshot,
    row_hashes: Vec<u64>,
}

impl TerminalInteraction {
    /// `observation` is the caller's argument; `None` selects this
    /// connection's latest observation. The order under the serial guard is
    /// fixed: observation/availability/pending fences → claim → pre-write
    /// capture and the remaining fences → write → release → readback.
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
            !(options.allow_output_below_cursor && action["type"] == "click"),
            "allow_output_below_cursor cannot admit a click; the layout must be current"
        );
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let terminal = resolved.binding.terminal_id.as_str();
        let client = self.client(identity, &resolved.binding).await?;
        // One action at a time per connection, readback wait included (up to
        // WAIT_MS_MAX): a second input from the same Planner on this terminal
        // queues here rather than writing into the screen the first one is
        // still waiting to read back. Other connections are not serialized.
        let _serial = {
            let _queued = client.queued_for_serial();
            client.serial.lock().await
        };
        // Write authority is decided under the serial lock: an input queued
        // behind a long readback must see the task/session state as it is
        // when its turn comes, not as it was when the call arrived. Checked
        // before the serial, a task that finished during the queue would be
        // answered with stale_observation although write authority is gone.
        Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, true).await?;
        let key = request_key.to_owned();
        // The fingerprint hashes the arguments as given (null when omitted) so
        // a replayed request_id returns the same receipt and never claims,
        // releases or writes again.
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
            // A replayed receipt's readback compares against the CURRENT
            // state (revision and signal seq at this call), not against the
            // state before the original write: the action already happened
            // and the caller is asking what changed from here on.
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
        // 1. The observation and the connection: binding, age, availability,
        //    no pending write.
        let saved = self.saved_observation(identity, &resolved, &client, observation)?;
        Self::ensure_writable(&client)?;
        // 2. Claim (#1666 S3): decided before the pre-write capture, since a
        //    granted claim changes what the control fence compares.
        let claim = match options.claim {
            true => Some(self.claim_for_input(&client, saved.control).await?),
            false => None,
        };
        if let Some(ClaimStep::Unavailable { status, reason }) = &claim {
            // No write and nothing cached: a resend after the human is done
            // must not conflict. The capture registers as the latest.
            let receipt =
                control_unavailable_receipt(terminal, request_key, observation, status, reason);
            return Ok(self
                .with_observation(identity, &client, receipt, Some(WaitPlan::default()), None)
                .await);
        }
        // 3. Pre-write capture and the remaining fences: control, live
        //    viewport, surface, action validity, revision (or a tolerance).
        let fence = self.pre_write_fences(&client, &saved, &action, options, claim.as_ref())?;
        let Ready {
            bytes,
            input_revision,
            signal_seq,
            tolerated,
        } = match fence {
            Fence::Ready(ready) => ready,
            Fence::Stale { current, diff } => {
                // No physical write and nothing cached under the request_id:
                // a later resend with another flag or observation must not
                // conflict. The capture registers as this connection's latest.
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
            if let Some(diff) = &tolerated {
                merge(&mut drift, diff.tolerance_json());
            }
            drift
        });
        let mut receipts = WriteReceipts::new(
            terminal,
            request_key,
            observation,
            drift.as_ref(),
            sequence_steps(&action),
        );
        receipts.attach(claim.as_ref());
        // 4. Write: reserve, cache the unknown receipt, send, await the ack.
        let mut result =
            write_action(&client, key.clone(), fingerprint.clone(), bytes, receipts).await?;
        // 5. Release (#1666 S3): after the write's outcome is known and
        //    cached; never clears `pending`, never rewrites the outcome. A
        //    call cancelled here leaves `requested` in the cached receipt and
        //    a replay never releases.
        if options.release {
            result["release"] = json!({"status":"requested"});
            cache(&client, &key, &fingerprint, &result).await;
            let status = self.release_after_input(&client).await;
            result["release"] = json!({"status":status});
            cache(&client, &key, &fingerprint, &result).await;
        }
        // 6. Readback against the pre-write baseline.
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
                && saved.created.elapsed() < Duration::from_secs(120),
            "observation belongs to another connection or expired"
        );
        Ok(Saved {
            revision: saved.revision,
            control: saved.control,
            surface: saved.surface,
            cursor: saved.cursor,
            row_hashes: saved.row_hashes.clone(),
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
    /// The fences that read the live screen. A granted claim authorizes the
    /// observer → owner transition explicitly (the equality fence is not
    /// re-run against the observer observation); otherwise the observation's
    /// control must be the one held now.
    fn pre_write_fences(
        &self,
        client: &Client,
        saved: &Saved,
        action: &Value,
        options: InputOptions,
        claim: Option<&ClaimStep>,
    ) -> Result<Fence> {
        Self::ensure_writable(client)?;
        let claimed = matches!(claim, Some(ClaimStep::Claimed(_)));
        let control = client
            .screen
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?
            .control;
        ensure!(
            claimed || (saved.control.is_some() && saved.control == control),
            "terminal control changed; observe before input"
        );
        ensure!(
            saved.surface.scroll_offset == 0,
            "return to live viewport before input"
        );
        // Read immediately before the physical write: this is the readback
        // baseline (revision and signal seq) and the drift evidence.
        let signal_seq = client.entry.signals.last_seq();
        let (frame, current) = client
            .entry
            .handle
            .model_view
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
            .capture(0)?;
        let now = frame.input_surface();
        if !same_input_surface(&saved.surface, &now) {
            let mut message = "terminal surface changed since observation (size, input modes or alternate screen); observe again".to_owned();
            if let Some(ClaimStep::Claimed(control)) = claim {
                // An RPC error carries no receipt: the caller still learns
                // that it now holds control.
                message.push_str(&format!("; control claimed (control_id {control})"));
            }
            anyhow::bail!(message);
        }
        // Encode against the live surface (proved equal to the saved one)
        // before deciding stale vs ready: an invalid action is an RPC
        // error whatever the revision did, so only the exact-revision
        // fence is relaxed by the stale result.
        let bytes = encode(action, &now)?;
        let ready = |tolerated| {
            Fence::Ready(Ready {
                bytes,
                input_revision: current,
                signal_seq,
                tolerated,
            })
        };
        if saved.revision == current || options.allow_output_since_observation {
            return Ok(ready(None));
        }
        // Every other fence passed and only the exact revision differs. The
        // row comparison (#1666 S4) says whether only rows strictly below an
        // unmoved cursor changed; it admits the write only on opt-in and is
        // reported on the stale result either way.
        let diff = ScreenDiff::compare(
            saved.cursor,
            &saved.row_hashes,
            CursorSnapshot::from(&frame.cursor),
            &row_hashes(&frame),
        );
        if options.allow_output_below_cursor && diff.only_below_cursor() {
            return Ok(ready(Some(diff)));
        }
        Ok(Fence::Stale { current, diff })
    }
}
/// Outcome of the pre-write fences: bytes to write with the live revision, or
/// a stale observation (only the exact-revision fence failed) that becomes a
/// structured refusal rather than an error.
enum Fence {
    Ready(Ready),
    Stale { current: u64, diff: ScreenDiff },
}
struct Ready {
    bytes: Vec<u8>,
    input_revision: u64,
    /// Signal seq read before the write.
    signal_seq: u64,
    /// The comparison that admitted a moved revision (`allow_output_below_cursor`).
    tolerated: Option<ScreenDiff>,
}
/// The three receipts a write can end with, built before the reservation so
/// the cached unknown receipt already carries every fact of the request.
struct WriteReceipts {
    unknown: Value,
    written: Value,
    refused: Value,
}
impl WriteReceipts {
    fn new(
        terminal: &str,
        request_key: &str,
        observation: Uuid,
        drift: Option<&Value>,
        steps: Option<usize>,
    ) -> Self {
        let mut receipts = Self {
            unknown: unknown_receipt(terminal, request_key, observation, drift),
            written: acknowledged_receipt(terminal, request_key, observation, drift, true),
            refused: acknowledged_receipt(terminal, request_key, observation, drift, false),
        };
        if let Some(steps) = steps {
            receipts.each(|receipt| receipt["steps"] = json!(steps));
        }
        receipts
    }
    fn attach(&mut self, claim: Option<&ClaimStep>) {
        self.each(|receipt| attach_claim(receipt, claim));
    }
    fn each(&mut self, mut apply: impl FnMut(&mut Value)) {
        apply(&mut self.unknown);
        apply(&mut self.written);
        apply(&mut self.refused);
    }
}
/// Reserve the next input sequence, cache the unknown receipt under the
/// request key, send ONE ordered write and await its acknowledgement or
/// refusal; the returned receipt is cached before it is returned.
/// Cancellation preserves Unknown and blocks all subsequent writes until the
/// matching ack/refusal is observed.
async fn write_action(
    client: &Client,
    key: String,
    fingerprint: String,
    bytes: Vec<u8>,
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
    let result = if client
        .send(ClientMsg::Input {
            data: bytes,
            input_seq: sequence,
        })
        .await
        .is_err()
    {
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
/// `claim` (and, once granted, `control_id`) on every result of a
/// `claim:true` request, so the caller knows what it holds even when the
/// write did not happen.
fn attach_claim(receipt: &mut Value, claim: Option<&ClaimStep>) {
    let Some(claim) = claim else {
        return;
    };
    receipt["claim"] = claim.to_json();
    if let ClaimStep::Claimed(control) = claim {
        receipt["control_id"] = json!(control);
    }
}
fn merge(target: &mut Value, fields: Value) {
    if let (Some(target), Some(fields)) = (target.as_object_mut(), fields.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
}
/// The stale-observation result: the request was not written, and the caller
/// is told what to compare and how to resend. `screen_diff` (#1666 S4) says
/// whether `allow_output_below_cursor` would admit the resend.
fn stale_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    observed_revision: u64,
    current_revision: u64,
    diff: &ScreenDiff,
) -> Value {
    json!({"terminal_id":terminal,"request_id":request_key,"outcome":"stale_observation",
        "application_result":"unverified","observation_id_used":observation,
        "observed_revision":observed_revision,"current_revision":current_revision,
        "screen_diff":diff.to_json(observed_revision, current_revision),
        "next":"inspect observation.state and screen_diff; if only rows below the cursor changed (cursor unmoved, rows_changed_at_or_above_cursor 0), resend the same request_id with allow_output_below_cursor=true; if only status text changed elsewhere, resend the same request_id with allow_output_since_observation=true; else act on the new state. Neither flag bypasses the control, surface, viewport or pending fences"})
}
/// The control-unavailable result (#1666 S3): `claim:true` could not put
/// control in this connection's hands, nothing was written or cached.
fn control_unavailable_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    status: &str,
    reason: &str,
) -> Value {
    json!({"terminal_id":terminal,"request_id":request_key,"outcome":"control_unavailable",
        "application_result":"unverified","observation_id_used":observation,
        "reason":reason,"claim":{"status":status,"reason":reason},
        "next":"nothing was written; inspect observation.state.role and control_id: when the terminal is free again, observe and resend with claim=true, otherwise wait for the human or ask before a deliberate calm.terminal.control claim"})
}
/// Every input receipt, whatever its outcome, carries
/// `application_result:"unverified"`: an acknowledgement says bytes reached the
/// PTY, an unknown outcome says not even that is known, and neither says what
/// the application did with them.
fn unknown_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
) -> Value {
    let mut receipt = json!({"terminal_id":terminal,"request_id":request_key,"outcome":"unknown","repeat_input":false,
        "application_result":"unverified","observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}
fn acknowledged_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
    written: bool,
) -> Value {
    let mut receipt = json!({"terminal_id":terminal,"request_id":request_key,"outcome":if written{"written"}else{"refused"},
        "application_result":"unverified","next":"observe the application result",
        "observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    fn diff() -> ScreenDiff {
        let at = CursorSnapshot {
            row: 1,
            column: 0,
            visible: true,
        };
        ScreenDiff::compare(at, &[1, 2, 3], at, &[1, 2, 9])
    }

    /// The field contract is uniform: written, refused and unknown receipts
    /// all say `application_result:"unverified"`; only acknowledged ones add
    /// `next`, and drift evidence is copied whenever it exists.
    #[test]
    fn every_terminal_input_receipt_outcome_reports_application_result_unverified() {
        let observation = Uuid::new_v4();
        let drift = json!({"observed_revision":3,"input_revision":5});
        let unknown = unknown_receipt("t1", "r1", observation, None);
        let written = acknowledged_receipt("t1", "r1", observation, None, true);
        let refused = acknowledged_receipt("t1", "r1", observation, Some(&drift), false);
        for (receipt, outcome) in [
            (&unknown, "unknown"),
            (&written, "written"),
            (&refused, "refused"),
        ] {
            assert_eq!(receipt["outcome"], outcome, "{receipt}");
            assert_eq!(receipt["application_result"], "unverified", "{receipt}");
            assert_eq!(receipt["terminal_id"], "t1");
            assert_eq!(receipt["request_id"], "r1");
            assert_eq!(receipt["observation_id_used"], json!(observation));
            assert!(receipt.get("application_completed").is_none());
        }
        assert_eq!(unknown["repeat_input"], false);
        assert!(unknown.get("next").is_none());
        assert_eq!(unknown["output_since_observation"], false);
        assert_eq!(written["next"], "observe the application result");
        assert_eq!(refused["output_since_observation"], true);
        assert_eq!(refused["observation_drift"], drift);
        assert_eq!(
            unknown_receipt("t1", "r1", observation, Some(&drift))["observation_drift"],
            drift
        );
        let stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        assert_eq!(stale["outcome"], "stale_observation");
        assert_eq!(stale["application_result"], "unverified");
        assert_eq!(stale["terminal_id"], "t1");
        assert_eq!(stale["request_id"], "r1");
        assert_eq!(stale["observation_id_used"], json!(observation));
        assert_eq!(stale["observed_revision"], 3);
        assert_eq!(stale["current_revision"], 5);
        let next = stale["next"].as_str().unwrap();
        assert!(
            next.contains("resend the same request_id with allow_output_since_observation=true")
        );
        assert!(next.contains("resend the same request_id with allow_output_below_cursor=true"));
        assert!(
            next.contains("Neither flag bypasses the control, surface, viewport or pending fences")
        );
        assert_eq!(
            stale["screen_diff"],
            json!({"compared":{"observed_revision":3,"current_revision":5},"cursor":{"moved":false,"visible":true},
                "rows_changed_total":1,"rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":1})
        );
        assert!(stale.get("output_since_observation").is_none());
        assert!(stale.get("observation_drift").is_none());
        let unavailable =
            control_unavailable_receipt("t1", "r1", observation, "unconfirmed", "why");
        assert_eq!(unavailable["outcome"], "control_unavailable");
        assert_eq!(unavailable["application_result"], "unverified");
        assert_eq!(unavailable["reason"], "why");
        assert_eq!(
            unavailable["claim"],
            json!({"status":"unconfirmed","reason":"why"})
        );
        assert!(
            unavailable["next"]
                .as_str()
                .unwrap()
                .contains("nothing was written")
        );
    }

    /// #1666: `steps` on every write receipt of a sequence, `claim` and
    /// `control_id` on every result of a claim, whatever the outcome.
    #[test]
    fn write_receipts_carry_steps_and_claim_uniformly() {
        let observation = Uuid::new_v4();
        let control = Uuid::new_v4();
        let mut receipts = WriteReceipts::new("t1", "r1", observation, None, Some(4));
        receipts.attach(Some(&ClaimStep::Claimed(control)));
        for receipt in [&receipts.unknown, &receipts.written, &receipts.refused] {
            assert_eq!(receipt["steps"], 4, "{receipt}");
            assert_eq!(
                receipt["claim"],
                json!({"status":"claimed","control_id":control})
            );
            assert_eq!(receipt["control_id"], json!(control));
        }
        let mut plain = WriteReceipts::new("t1", "r1", observation, None, None);
        plain.attach(Some(&ClaimStep::Held));
        assert!(plain.written.get("steps").is_none());
        assert_eq!(plain.written["claim"], json!({"status":"held"}));
        assert!(plain.written.get("control_id").is_none());
        let mut none = WriteReceipts::new("t1", "r1", observation, None, None);
        none.attach(None);
        assert!(none.written.get("claim").is_none());
        let mut stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        attach_claim(&mut stale, Some(&ClaimStep::Claimed(control)));
        assert_eq!(stale["claim"]["status"], "claimed");
        assert_eq!(stale["control_id"], json!(control));
        let mut drift = json!({"observed_revision":3,"input_revision":5});
        merge(&mut drift, diff().tolerance_json());
        assert_eq!(
            drift,
            json!({"observed_revision":3,"input_revision":5,"tolerance":"below_cursor",
                "rows_changed_below_cursor":[2],"rows_changed_total":1,"truncated":false})
        );
    }
}
