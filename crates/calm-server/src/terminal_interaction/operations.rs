use super::*;
use calm_terminal_view::{click_bytes, key_bytes};

impl TerminalInteraction {
    /// `observation` is the caller's argument; `None` selects this
    /// connection's latest observation. `allow_output_since_observation`
    /// replaces the exact-revision fence with a same-surface fence.
    #[allow(clippy::too_many_arguments)]
    pub async fn input(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        observation: Option<Uuid>,
        request_key: &str,
        action: Value,
        allow_output_since_observation: bool,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Value> {
        if let Some(wait) = observation_wait {
            wait.validate()?;
        }
        ensure!(
            !request_key.is_empty() && request_key.len() <= 128,
            "invalid input request key"
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
        // The fingerprint hashes the argument as given (null when omitted) so
        // a replayed request_id returns the same receipt.
        let fingerprint = crate::routes::terminal_cards::stable_payload_hash(&json!({
            "observation_id":observation,"action":action,
            "allow_output_since_observation":allow_output_since_observation
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
            return Ok(self
                .with_observation(identity, &client, receipt, observation_wait, None)
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
        let fence = {
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
            let state = client.screen.lock().unwrap();
            ensure!(
                state.available && !state.exited && state.pending.is_none(),
                "terminal unavailable or prior input outcome unknown"
            );
            ensure!(
                saved.control.is_some() && saved.control == state.control,
                "terminal control changed; observe before input"
            );
            ensure!(
                saved.surface.scroll_offset == 0,
                "return to live viewport before input"
            );
            // Read immediately before the physical write: this is the readback
            // baseline and the drift evidence.
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
            // Encode against the live surface (proved equal to the saved one)
            // before deciding stale vs ready: an invalid action is an RPC
            // error whatever the revision did, so only the exact-revision
            // fence is relaxed by the stale result.
            let bytes = encode(&action, &now)?;
            if !allow_output_since_observation && saved.revision != current {
                // Every other fence passed and only the exact revision differs:
                // a structured result with a fresh observation instead of an
                // error, so the caller can decide without a separate observe.
                Fence::Stale {
                    observed: saved.revision,
                    current,
                }
            } else {
                Fence::Ready(bytes, saved.revision, current)
            }
        };
        let (bytes, observed_revision, input_revision) = match fence {
            Fence::Ready(bytes, observed, current) => (bytes, observed, current),
            Fence::Stale { observed, current } => {
                // No physical write and nothing cached under the request_id:
                // a later resend with another flag or observation must not
                // conflict. The capture registers as this connection's latest.
                let receipt = stale_receipt(terminal, request_key, observation, observed, current);
                return Ok(self
                    .with_observation(identity, &client, receipt, Some(WaitPlan::default()), None)
                    .await);
            }
        };
        let drift = if input_revision != observed_revision {
            Some(json!({"observed_revision":observed_revision,"input_revision":input_revision}))
        } else {
            None
        };
        // Reserve before enqueue. Cancellation preserves Unknown and blocks all
        // subsequent writes until the matching ack/refusal is observed.
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
        let unknown = unknown_receipt(terminal, request_key, observation, drift.as_ref());
        client
            .requests
            .lock()
            .await
            .insert(key.clone(), (fingerprint.clone(), unknown.clone()));
        let result = if client
            .send(ClientMsg::Input {
                data: bytes,
                input_seq: sequence,
            })
            .await
            .is_err()
        {
            unknown
        } else {
            match client
                .wait(
                    |state| state.ack >= sequence || state.refused >= sequence,
                    Duration::from_secs(7),
                )
                .await
            {
                Ok(()) => {
                    let written = client.screen.lock().unwrap().ack >= sequence;
                    acknowledged_receipt(
                        terminal,
                        request_key,
                        observation,
                        drift.as_ref(),
                        written,
                    )
                }
                Err(_) => unknown,
            }
        };
        client
            .requests
            .lock()
            .await
            .insert(key, (fingerprint, result.clone()));
        Ok(self
            .with_observation(
                identity,
                &client,
                result,
                observation_wait,
                Some(input_revision),
            )
            .await)
    }
}
/// Outcome of the pre-write fences: bytes to write with the observed and
/// live revisions, or a stale observation (only the exact-revision fence
/// failed) that becomes a structured refusal rather than an error.
enum Fence {
    Ready(Vec<u8>, u64, u64),
    Stale { observed: u64, current: u64 },
}
/// The stale-observation result: the request was not written, and the caller
/// is told what to compare and how to resend.
fn stale_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    observed_revision: u64,
    current_revision: u64,
) -> Value {
    json!({"terminal_id":terminal,"request_id":request_key,"outcome":"stale_observation",
        "application_result":"unverified","observation_id_used":observation,
        "observed_revision":observed_revision,"current_revision":current_revision,
        "next":"inspect observation.state; if only status text changed, resend the same request_id with allow_output_since_observation=true, else act on the new state"})
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
fn encode(action: &Value, frame: &InputSurface) -> Result<Vec<u8>> {
    let object = action
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("terminal action must be an object"))?;
    match action["type"].as_str() {
        Some("text") => {
            ensure!(object.len() == 2, "text action accepts only type/text");
            let text = action["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("text required"))?;
            ensure!(
                text.len() <= 16384 && !text.is_empty() && !text.chars().any(char::is_control),
                "text must be nonempty printable text; use explicit keys for Enter or controls"
            );
            Ok(text.as_bytes().to_vec())
        }
        Some("key") => {
            ensure!(
                (2..=3).contains(&object.len())
                    && object
                        .keys()
                        .all(|field| matches!(field.as_str(), "type" | "key" | "repeat")),
                "key action accepts only type/key/repeat"
            );
            let key = action["key"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("key required"))?;
            let repeat = match object.get("repeat") {
                None => 1,
                Some(value) => value
                    .as_u64()
                    .filter(|value| (1..=32).contains(value))
                    .ok_or_else(|| anyhow::anyhow!("repeat must be an integer from 1 to 32"))?,
            };
            ensure!(
                repeat == 1
                    || matches!(
                        key,
                        "Left" | "Right" | "Up" | "Down" | "Backspace" | "Delete"
                    ),
                "only navigation and editing keys may repeat"
            );
            // One bounded action, one receipt and one physical ownership barrier.
            // Never turn Enter, Escape or control keys into repeated submissions.
            Ok(key_bytes(key, frame.modes)?.repeat(repeat as usize))
        }
        Some("click") => {
            ensure!(object.len() == 3, "click accepts only type/column/row");
            let coordinate = |field: &str| {
                action[field]
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| anyhow::anyhow!("invalid cell coordinate"))
            };
            click_bytes(coordinate("column")?, coordinate("row")?, frame)
        }
        _ => anyhow::bail!("unknown terminal action"),
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

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
        let stale = stale_receipt("t1", "r1", observation, 3, 5);
        assert_eq!(stale["outcome"], "stale_observation");
        assert_eq!(stale["application_result"], "unverified");
        assert_eq!(stale["terminal_id"], "t1");
        assert_eq!(stale["request_id"], "r1");
        assert_eq!(stale["observation_id_used"], json!(observation));
        assert_eq!(stale["observed_revision"], 3);
        assert_eq!(stale["current_revision"], 5);
        assert!(
            stale["next"]
                .as_str()
                .unwrap()
                .contains("allow_output_since_observation=true")
        );
        assert!(stale.get("output_since_observation").is_none());
        assert!(stale.get("observation_drift").is_none());
    }
}
