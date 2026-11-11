//! Guest work crosses the real dispatch owner; other provenance remains pending.
use super::*;
use crate::conversation_inputs::{InputSource, ShareInputSource};
use calm_types::share_links::ShareScope;

#[tokio::test]
async fn conversation_inputs_share_guest_dispatch_keeps_owner_supplement_pending() {
    let fx = Fixture::with_claim(false).await;
    let track = fx.repo.card_get(&fx.card).await.unwrap().unwrap().track_id;
    let link = crate::share_access::create(
        fx.repo.as_ref(),
        fx.source.clone(),
        ShareScope::Track(track.to_string()),
    )
    .await
    .unwrap();
    let auth = crate::auth::ValidatedApplicationAuth::from_state(fx.auth.clone()).unwrap();
    let share = ShareInputSource::new(auth.execution, link.link.delegation);
    let claimed = crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let (card, message, share) = (fx.card.clone(), fx.message.clone(), share.clone());
        move |tx| {
            Box::pin(async move {
                let mut carrier = current_input_content_carrier_tx(tx, &card).await?;
                mutate_pending_human_tx(
                    tx,
                    &mut carrier,
                    LEGACY_OWNER_USER_ID,
                    &message,
                    PendingHumanChange::Withdraw,
                )
                .await?;
                accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: share.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "guest dispatch".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?;
                let plan =
                    pending_plan_for_prefix_tx(tx, &carrier, std::num::NonZeroU32::MIN, vec![])
                        .await?;
                crate::conversation_inputs::claim_initial_tx(
                    tx,
                    InputSource::Share(share),
                    &mut carrier,
                    plan,
                )
                .await
            })
        }
    })
    .await
    .unwrap();
    crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let id = share.delegation().id.clone();
        move |tx| {
            Box::pin(async move {
                crate::db::sqlite::share_links::revoke_tx(tx, &id, crate::model::now_ms()).await?;
                Ok(())
            })
        }
    })
    .await
    .unwrap();
    let report = initial(
        fx.repo.as_ref(),
        InputSource::Share(share),
        &fx.card,
        &fx.runtime,
        &claimed.submission_id,
        &fx.backend,
        &TurnModelSelection::inherit(),
        PlannerPermissionMode::Never,
        &fx.backend.handoff_for_test().await,
    )
    .await
    .unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Accepted(_)
    ));
    let owner = crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let (card, worker, submission) = (
            fx.card.clone(),
            fx.runtime.clone(),
            claimed.submission_id.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_content_carrier_tx(tx, &card).await?;
                let fence = RuntimeExecutionFence::from_carrier(&carrier);
                let turn = carrier
                    .checkpoint()
                    .attempt(&submission)
                    .unwrap()
                    .correlation
                    .native_turn_id
                    .clone()
                    .unwrap();
                let mut header = fence.header().clone();
                header["phase"] = serde_json::json!(HarnessPhaseTag::TurnRunning);
                header["last_turn_id"] = serde_json::json!(turn);
                persist_runtime_frame_tx(tx, &card, &worker, &fence, header).await?;
                let mut carrier = current_input_content_carrier_tx(tx, &card).await?;
                let accepted = accept_pending_human_tx(
                    tx,
                    &mut carrier,
                    LEGACY_OWNER_USER_ID,
                    HumanPayload {
                        text: "next owner turn".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?;
                let AcceptedInputRef::Human { message } = accepted.input_ref else {
                    panic!("Human")
                };
                Ok(message)
            })
        }
    })
    .await
    .unwrap();
    let checkpoint = fx.checkpoint().await;
    let turn = checkpoint
        .attempt(&claimed.submission_id)
        .unwrap()
        .correlation
        .native_turn_id
        .clone()
        .unwrap();
    let result = supplemental(
        fx.repo.as_ref(),
        fx.source.clone(),
        &fx.card,
        &fx.runtime,
        owner.clone(),
        turn,
        &fx.backend,
    )
    .await;
    assert!(
        matches!(result, Err(CalmError::Forbidden(_))),
        "{:?}",
        result.as_ref().err()
    );
    let after = fx.checkpoint().await;
    assert_eq!(after.pending, checkpoint.pending);
    assert_eq!(after.active, checkpoint.active);
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
}
