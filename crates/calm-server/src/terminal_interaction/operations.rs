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
        observation_wait: Option<WaitSpec>,
    ) -> Result<Value> {
        if let Some(wait) = observation_wait {
            wait.validate()?;
        }
        ensure!(
            !request_key.is_empty() && request_key.len() <= 128,
            "invalid input request key"
        );
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, true).await?;
        let terminal = resolved.binding.terminal_id.as_str();
        let client = self.client(identity, &resolved.binding).await?;
        let _serial = client.serial.lock().await;
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
                .map(|(id, _)| id)
                .ok_or_else(|| {
                    anyhow::anyhow!("no observation on this connection; observe first")
                })?,
        };
        let (bytes, observed_revision, input_revision) = {
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
            let surface = if allow_output_since_observation {
                let now = frame.input_surface();
                ensure!(
                    saved.surface.cols == now.cols
                        && saved.surface.rows == now.rows
                        && saved.surface.modes == now.modes,
                    "terminal surface changed since observation (size or input modes); observe again"
                );
                now
            } else {
                ensure!(
                    saved.revision == current,
                    "terminal changed since observation; observe again"
                );
                saved.surface
            };
            (encode(&action, &surface)?, saved.revision, current)
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
        let mut unknown = json!({"terminal_id":terminal,"request_id":request_key,"outcome":"unknown","repeat_input":false,
            "observation_id_used":observation});
        if let Some(drift) = &drift {
            unknown["output_since_observation"] = json!(true);
            unknown["observation_drift"] = drift.clone();
        } else {
            unknown["output_since_observation"] = json!(false);
        }
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
                    let state = client.screen.lock().unwrap();
                    let mut receipt = json!({"terminal_id":terminal,"request_id":request_key,"outcome":if state.ack>=sequence{"written"}else{"refused"},
                        "application_result":"unverified","next":"observe the application result",
                        "observation_id_used":observation,"output_since_observation":drift.is_some()});
                    if let Some(drift) = drift {
                        receipt["observation_drift"] = drift;
                    }
                    receipt
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
