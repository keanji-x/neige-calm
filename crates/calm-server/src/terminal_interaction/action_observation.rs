//! Optional readback never changes the physical action receipt or attaches a client.
use super::*;

impl TerminalInteraction {
    pub(super) async fn with_observation(
        &self,
        identity: &ToolCallIdentity,
        client: &Arc<Client>,
        mut receipt: Value,
        wait: Option<WaitSpec>,
        baseline: Option<u64>,
    ) -> Value {
        let Some(wait) = wait else {
            return receipt;
        };
        let captured = async {
            // Pin the physical client and its complete execution binding to
            // the action that already ran; `capture` re-reads task/control
            // state again after its wait, against this same binding.
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
            self.capture(
                identity,
                resolved,
                client,
                0,
                wait,
                baseline,
                ObservationFormat::Text,
            )
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

/// Release readback economy (#1618): a release rarely changes the screen, so
/// when the readback captured the same revision as this connection's previous
/// observation (`previous`, read before the readback registered itself) the
/// `text` array is dropped and `text_omitted` names that observation. Every
/// other field stays; claim readbacks and observe always carry text.
pub(super) fn omit_unchanged_release_text(receipt: &mut Value, previous: Option<(Uuid, u64)>) {
    let Some((id, revision)) = previous else {
        return;
    };
    if receipt["observation"]["status"] != "available"
        || receipt["observation"]["state"]["observation_revision"] != json!(revision.to_string())
    {
        return;
    }
    let Some(state) = receipt
        .get_mut("observation")
        .and_then(|observation| observation.get_mut("state"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    state.remove("text");
    state.insert(
        "text_omitted".into(),
        json!(format!("unchanged since previous observation {id}")),
    );
}

#[cfg(test)]
mod release_text_tests {
    use super::*;

    #[test]
    fn release_readback_omits_text_only_for_the_unchanged_revision() {
        let id = Uuid::new_v4();
        let state = json!({"observation_id":Uuid::new_v4(),"observation_revision":"9","text":["$ "],"role":"observer"});
        let receipt = || json!({"terminal_id":"t1","control_id":null,"observation":{"status":"available","state":state}});
        let mut same = receipt();
        omit_unchanged_release_text(&mut same, Some((id, 9)));
        assert!(same["observation"]["state"].get("text").is_none(), "{same}");
        assert_eq!(
            same["observation"]["state"]["text_omitted"],
            json!(format!("unchanged since previous observation {id}"))
        );
        assert_eq!(same["observation"]["state"]["observation_revision"], "9");
        assert_eq!(same["observation"]["state"]["role"], "observer");
        assert_eq!(same["control_id"], Value::Null);
        let mut moved = receipt();
        omit_unchanged_release_text(&mut moved, Some((id, 8)));
        assert_eq!(moved, receipt());
        let mut first = receipt();
        omit_unchanged_release_text(&mut first, None);
        assert_eq!(first, receipt());
        // An unavailable readback is left exactly as it was: no `state` key
        // may be conjured into it.
        let unavailable = json!({"terminal_id":"t1","control_id":null,"observation":{"status":"unavailable","reason":"gone"}});
        let mut untouched = unavailable.clone();
        omit_unchanged_release_text(&mut untouched, Some((id, 9)));
        assert_eq!(untouched, unavailable);
        let mut bare = json!({"terminal_id":"t1","control_id":null});
        omit_unchanged_release_text(&mut bare, Some((id, 9)));
        assert_eq!(bare, json!({"terminal_id":"t1","control_id":null}));
    }
}
