//! Optional readback never changes the physical action receipt or attaches a client.
use super::*;

impl TerminalInteraction {
    pub(super) async fn with_observation(
        &self,
        identity: &ToolCallIdentity,
        client: &Arc<Client>,
        mut receipt: Value,
        wait_ms: Option<u64>,
    ) -> Value {
        let Some(wait_ms) = wait_ms else {
            return receipt;
        };
        let captured = async {
            if wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            // Re-read task/control state, but pin the physical client and its
            // complete execution binding to the action that already ran.
            let resolved = Self::resolve_target(
                self.repo.as_ref(),
                identity,
                &Target::Terminal(client.binding.terminal_id.clone()),
            )
            .await?;
            ensure!(
                resolved.binding == client.binding,
                "action terminal binding changed before observation"
            );
            ensure!(
                self.renderer
                    .get(&client.binding.terminal_id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry, &client.entry)),
                "action terminal generation changed before observation"
            );
            self.capture(identity, resolved, client, 0, 0, ObservationFormat::Text)
                .await
        }
        .await;
        receipt["observation"] = match captured {
            Ok((state, _)) => json!({"status":"available","state":state}),
            Err(error) => json!({"status":"unavailable","reason":error.to_string()}),
        };
        receipt
    }
}
