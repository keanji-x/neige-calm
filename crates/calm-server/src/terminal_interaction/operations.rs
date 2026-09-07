use super::*;
use calm_terminal_view::{click_bytes, key_bytes};

impl TerminalInteraction {
    pub async fn input(
        &self,
        identity: &ToolCallIdentity,
        terminal: &str,
        observation: Uuid,
        request_key: &str,
        action: Value,
    ) -> Result<Value> {
        ensure!(
            !request_key.is_empty() && request_key.len() <= 128,
            "invalid input request key"
        );
        let client = self.client(identity, terminal).await?;
        let _serial = client.serial.lock().await;
        let key = request_key.to_owned();
        let fingerprint = crate::routes::terminal_cards::stable_payload_hash(
            &json!({"observation_id":observation,"action":action}),
        )?;
        {
            let requests = client.requests.lock().await;
            if let Some((prior, result)) = requests.get(&key) {
                ensure!(
                    prior == &fingerprint,
                    "input request key reused with different arguments"
                );
                return Ok(result.clone());
            }
            ensure!(
                requests.len() < 4096,
                "terminal connection receipt limit reached; detach and observe a fresh connection"
            );
        }
        let bytes = {
            let observations = self
                .observations
                .lock()
                .map_err(|_| anyhow::anyhow!("observation registry poisoned"))?;
            let saved = observations
                .get(&observation)
                .ok_or_else(|| anyhow::anyhow!("observation expired; observe again"))?;
            ensure!(
                saved.binding == Self::binding(identity, terminal)
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
                saved.revision
                    == client
                        .entry
                        .handle
                        .model_view
                        .lock()
                        .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
                        .capture(0)?
                        .1,
                "terminal changed since observation; observe again"
            );
            ensure!(
                saved.surface.scroll_offset == 0,
                "return to live viewport before input"
            );
            encode(&action, &saved.surface)?
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
        let unknown = json!({"terminal_id":terminal,"request_id":request_key,"outcome":"unknown","repeat_input":false});
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
                    json!({"terminal_id":terminal,"request_id":request_key,"outcome":if state.ack>=sequence{"written"}else{"refused"},
                        "application_completed":false,"next":"observe the application result"})
                }
                Err(_) => unknown,
            }
        };
        client
            .requests
            .lock()
            .await
            .insert(key, (fingerprint, result.clone()));
        Ok(result)
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
            ensure!(object.len() == 2, "key action accepts only type/key");
            key_bytes(
                action["key"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("key required"))?,
                frame.modes,
            )
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
