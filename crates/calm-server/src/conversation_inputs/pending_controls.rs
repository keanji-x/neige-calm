//! Authenticated pending-content changes share canonical acceptance's current author policy.
use super::*;
use crate::db::{RepoEventWrite, write_in_tx_typed};
use calm_truth::db::sqlite::conversation::{PendingHumanChange, PendingHumanWrite};
use calm_types::conversation::MessageRevisionRef;

pub(crate) struct PendingHumanMutation {
    pub card: String,
    pub worker: String,
    pub expected: MessageRevisionRef,
    pub change: PendingHumanChange,
}

pub(crate) async fn mutate(
    repo: &dyn RepoEventWrite,
    registry: crate::harness::registry::HarnessRegistry,
    source: HumanInputSource,
    mutation: PendingHumanMutation,
) -> Result<(
    PendingHumanWrite,
    Option<crate::event::BroadcastEnvelope>,
    crate::ids::TrackId,
    crate::harness::registry::SourceHandoff,
)> {
    let PendingHumanMutation {
        card,
        worker,
        expected,
        change,
    } = mutation;
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            let area = authorize_human_tx(tx, &principal, &card).await?;
            let mut carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
            if carrier.worker_session_id() != worker {
                return Err(CalmError::Conflict("conversation carrier changed".into()));
            }
            let runtime = crate::db::sqlite::session_get_tx(tx, &worker.clone().into())
                .await?
                .ok_or_else(|| CalmError::NotFound("conversation runtime".into()))?;
            let is_edit = matches!(&change, PendingHumanChange::Edit { .. });
            let result = conversation::mutate_pending_human_tx(
                tx,
                &mut carrier,
                &principal.user_id,
                &expected,
                change,
            )
            .await?;
            source.current_principal()?;
            let envelope = if matches!(&result, PendingHumanWrite::Applied { .. }) {
                let event = crate::event::Event::HarnessQueueChanged {
                    worker_session_id: worker,
                    card_id: card.clone().into(),
                    track_id: runtime.track_id.clone(),
                    entry_id: expected.message_id.to_string(),
                    actor: crate::ids::ActorId::User,
                    change: if is_edit {
                        calm_types::event::HarnessQueueChange::Edited
                    } else {
                        calm_types::event::HarnessQueueChange::Deleted
                    },
                };
                let scope = crate::event::EventScope::Card {
                    card: card.into(),
                    track: runtime.track_id.clone(),
                    area: area.id,
                };
                let id = crate::db::sqlite::append_decision_event_in_tx(
                    tx,
                    &crate::ids::ActorId::User,
                    &scope,
                    None,
                    &event,
                )
                .await?;
                Some(crate::event::BroadcastEnvelope {
                    id,
                    event_version: crate::event::SYNC_EVENT_VERSION,
                    actor: crate::ids::ActorId::User,
                    scope,
                    event,
                })
            } else {
                None
            };
            let handoff = registry
                .canonical_sources(&runtime.id.0, &runtime.track_id)
                .begin_handoff();
            Ok((result, envelope, runtime.track_id, handoff))
        })
    })
    .await
}
