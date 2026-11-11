//! Cold recovery uses the installed deployment issuer, never a visitor session.
use super::*;
use crate::conversation_inputs::{InputSource, ShareInputSource};
use crate::db::{sqlite::conversation, write_in_tx_typed};
use calm_types::conversation::{
    MessageAuthor, PendingInput, SubmissionRenderInput, SubmissionRenderSegment,
};

impl Inner {
    pub(super) async fn reconcile_share_inputs(self: &Arc<Self>) -> crate::error::Result<()> {
        let producer = self.producer()?;
        let db = self.repo.sqlite_db().ok_or_else(|| {
            crate::error::CalmError::Internal("share recovery requires SQLite".into())
        })?;
        let cards: Vec<String> = sqlx::query_scalar(
            "SELECT c.id FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id
             WHERE EXISTS(SELECT 1 FROM conversation_messages m WHERE m.card_id=c.id AND m.author_kind='link_guest')
               AND json_valid(ws.handle_state_json) AND
               (json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')>0
                OR json_extract(ws.handle_state_json,'$.phase')='issuing_turn')")
            .fetch_all(db.read_pool()).await?;
        for card in cards {
            let sources = write_in_tx_typed(self.repo.as_ref(), {
                let (card, execution, harness) = (card.clone(), producer.execution.clone(), self.harness.clone());
                move |tx| Box::pin(async move {
                    let carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                    let mut ids = std::collections::BTreeSet::new();
                    for pending in &carrier.checkpoint().pending {
                        if let PendingInput::Human { message, .. } = pending
                            && let MessageAuthor::LinkGuest { share_link_id } = conversation::message_tx(tx, &message.message_id).await?.author {
                            ids.insert(share_link_id);
                        }
                    }
                    let mut delegations = std::collections::BTreeMap::new();
                    if carrier.phase()? == crate::harness::HarnessPhaseTag::IssuingTurn
                        && let Some(active) = &carrier.checkpoint().active
                        && let [id] = active.submission_ids.as_slice()
                        && carrier.checkpoint().attempt(id).is_none() {
                        let identity = conversation::submission_identity_tx(tx, id).await?;
                        if let calm_types::conversation::ConversationExecutionProvenance::ShareDelegation { delegation, .. } = identity.execution {
                            let plan = conversation::submission_plan_tx(tx, id).await?;
                            if plan.plan.segments().iter().any(|segment| matches!(segment, SubmissionRenderSegment::Input { input: SubmissionRenderInput::LinkGuest { .. }, .. })) {
                                delegations.insert(delegation.id.clone(), delegation);
                            }
                        }
                    }
                    for id in ids {
                        if let std::collections::btree_map::Entry::Vacant(entry) = delegations.entry(id) {
                            // Missing/deleted approval remains held; the immutable history stays.
                            match crate::db::sqlite::share_links::get_tx(tx, entry.key()).await {
                                Ok(link) => { entry.insert(link.delegation); },
                                Err(calm_truth::TruthError::Core(calm_types::error::CoreError::NotFound(_))) => {},
                                Err(error) => return Err(error.into()),
                            }
                        }
                    }
                    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1").bind(&card).fetch_one(&mut **tx).await?;
                    let handoff = harness.canonical_sources(carrier.worker_session_id(), &track.into()).begin_handoff();
                    Ok((delegations.into_values().map(|delegation| InputSource::Share(ShareInputSource::new(execution.clone(), delegation))).collect::<Vec<_>>(), handoff))
                })
            }).await;
            match sources {
                Ok((sources, handoff)) => {
                    let handoff = Arc::new(handoff);
                    for source in sources {
                        let wake = self
                            .planner_wake
                            .read()
                            .expect("Planner wake context")
                            .clone();
                        let (card, handoff) = (card.clone(), handoff.clone());
                        tokio::spawn(async move {
                            let _handoff = handoff;
                            if let Some(wake) = wake
                                && let Err(error) =
                                    wake.wake_canonical(&card.clone().into(), source).await
                            {
                                tracing::debug!(%card, %error, "accepted share input remains held");
                            }
                        });
                    }
                }
                Err(error) => tracing::debug!(%card, %error, "share recovery remains held"),
            }
        }
        Ok(())
    }
}
