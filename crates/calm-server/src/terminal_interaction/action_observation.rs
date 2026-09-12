//! Optional readback never changes the physical action receipt or attaches a client.
use super::*;

impl TerminalInteraction {
    pub(super) async fn with_observation(
        &self,
        identity: &ToolCallIdentity,
        client: &Arc<Client>,
        mut receipt: Value,
        wait: Option<WaitPlan>,
        baseline: Option<ReadbackBaseline>,
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
/// `text` array is dropped and `text_omitted` names that observation. Both
/// captures must be live viewports (scroll offset 0): a history view shares
/// the live revision but shows different text, so eliding after one would
/// hide the live screen. Every other field stays; claim readbacks and observe
/// always carry text.
pub(super) fn omit_unchanged_release_text(
    receipt: &mut Value,
    previous: Option<LatestObservation>,
) {
    let Some(LatestObservation {
        id,
        revision,
        scroll_offset: 0,
        last_seq: _,
    }) = previous
    else {
        return;
    };
    if receipt["observation"]["status"] != "available"
        || receipt["observation"]["state"]["observation_revision"] != json!(revision.to_string())
        || receipt["observation"]["state"]["scroll_offset"] != json!(0)
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

    fn latest(id: Uuid, revision: u64, scroll_offset: usize) -> Option<LatestObservation> {
        Some(LatestObservation {
            id,
            revision,
            scroll_offset,
            last_seq: 0,
        })
    }

    #[test]
    fn release_readback_omits_text_only_for_the_unchanged_live_revision() {
        let id = Uuid::new_v4();
        let state = json!({"observation_id":Uuid::new_v4(),"observation_revision":"9","scroll_offset":0,"text":["$ "],"role":"observer"});
        let receipt = || json!({"terminal_id":"t1","control_id":null,"observation":{"status":"available","state":state}});
        let mut same = receipt();
        omit_unchanged_release_text(&mut same, latest(id, 9, 0));
        assert!(same["observation"]["state"].get("text").is_none(), "{same}");
        assert_eq!(
            same["observation"]["state"]["text_omitted"],
            json!(format!("unchanged since previous observation {id}"))
        );
        assert_eq!(same["observation"]["state"]["observation_revision"], "9");
        assert_eq!(same["observation"]["state"]["role"], "observer");
        assert_eq!(same["control_id"], Value::Null);
        let mut moved = receipt();
        omit_unchanged_release_text(&mut moved, latest(id, 8, 0));
        assert_eq!(moved, receipt());
        let mut first = receipt();
        omit_unchanged_release_text(&mut first, None);
        assert_eq!(first, receipt());
        // The previous observation was a history view of the same revision:
        // its text is not the live text, so the readback keeps its own.
        let mut history = receipt();
        omit_unchanged_release_text(&mut history, latest(id, 9, 1));
        assert_eq!(history, receipt());
        // A readback that is itself a history view is never elided.
        let scrolled = json!({"terminal_id":"t1","control_id":null,"observation":{"status":"available",
            "state":{"observation_id":Uuid::new_v4(),"observation_revision":"9","scroll_offset":2,"text":["old"]}}});
        let mut kept = scrolled.clone();
        omit_unchanged_release_text(&mut kept, latest(id, 9, 0));
        assert_eq!(kept, scrolled);
        // An unavailable readback is left exactly as it was: no `state` key
        // may be conjured into it.
        let unavailable = json!({"terminal_id":"t1","control_id":null,"observation":{"status":"unavailable","reason":"gone"}});
        let mut untouched = unavailable.clone();
        omit_unchanged_release_text(&mut untouched, latest(id, 9, 0));
        assert_eq!(untouched, unavailable);
        let mut bare = json!({"terminal_id":"t1","control_id":null});
        omit_unchanged_release_text(&mut bare, latest(id, 9, 0));
        assert_eq!(bare, json!({"terminal_id":"t1","control_id":null}));
    }
}
