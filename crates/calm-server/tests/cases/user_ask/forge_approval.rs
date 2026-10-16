use super::*;
use calm_server::ask::{ForgeApproval, ForgeApprovalRead, forge_approval_tx};
use calm_server::event::AskAction;
use calm_server::ids::ActorId;

const KEY: &str = "gitforge:track:card:gh.run.rerun_failed:v1:[\"o/r\",100,1,\"r-1\"]";
const HASH: &str = "payload-hash";

/// What the forge gate reads for `(KEY, HASH)` as `card`, and the ask it raises when there is
/// none yet, written as the card's own like the gate writes it. `None` when it raised one.
async fn gate(boot: &Boot, card: &CardId) -> calm_server::error::Result<Option<ForgeApproval>> {
    let (track, card) = (boot.track_id.clone(), card.clone());
    let read = calm_server::db::write_in_tx_typed(boot.repo.as_ref(), {
        let (track, card) = (track.clone(), card.clone());
        move |tx| {
            Box::pin(async move {
                Ok(
                    match forge_approval_tx(tx, &track, &card, KEY, HASH, "Re-run?").await? {
                        ForgeApprovalRead::Recorded(approval) => Some(approval),
                        ForgeApprovalRead::Ask(..) => None,
                    },
                )
            })
        }
    })
    .await?;
    if read.is_some() {
        return Ok(read);
    }
    calm_server::db::write_with_actor_events_typed::<(), _>(
        boot.repo.as_ref(),
        None,
        &boot.events,
        &boot.ctx.write,
        move |tx| {
            Box::pin(async move {
                let ForgeApprovalRead::Ask(scope, event) =
                    forge_approval_tx(tx, &track, &card, KEY, HASH, "Re-run?").await?
                else {
                    panic!("no ask yet");
                };
                Ok(((), vec![(ActorId::AiPlanner(card), scope, *event)]))
            })
        },
    )
    .await?;
    Ok(None)
}

async fn raise(boot: &Boot) -> i64 {
    assert_eq!(gate(boot, &boot.planner_card_id).await.unwrap(), None);
    let asks = rows(boot, "ask.requested").await;
    let [(actor, Event::AskRequested { action, .. })] = asks.as_slice() else {
        panic!("one ask: {asks:?}");
    };
    assert_eq!(actor, "AiPlanner");
    assert_eq!(
        action,
        &Some(AskAction::ForgeAction {
            idempotency_key: KEY.into(),
            payload_hash: HASH.into(),
        })
    );
    let items = serde_json::to_value(activity_items(boot).await).unwrap();
    let id = items[0]["ask_id"].as_i64().unwrap();
    assert_eq!(
        items[0]["questions"][0]["options"],
        json!(["Approve", "Decline"])
    );
    id
}

#[tokio::test]
async fn forge_approval_ask_rejects_agents_and_invalid_contract() {
    let boot = boot().await;
    let id = raise(&boot).await;
    assert_eq!(
        gate(&boot, &boot.planner_card_id).await.unwrap(),
        Some(ForgeApproval::Awaiting { ask_id: id }),
        "an open ask is returned again, not raised twice"
    );
    // Only the user answers.
    assert_eq!(
        post_answer(
            &boot,
            boot.track_id.as_str(),
            id,
            json!({"answers":[{"option":0}]}),
            Some("ai:planner")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Only the track's Planner submits an approval-gated call.
    let worker = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        worker.id.as_str(),
        CardRole::Worker,
    )
    .await;
    assert!(matches!(
        gate(&boot, &worker.id).await,
        Err(CalmError::Forbidden(_))
    ));

    // A forge_action ask whose question is not the kernel's contract grants nothing: its answer
    // is refused.
    let (scope_track, scope_area) = (boot.track_id.clone(), boot.area_id.clone());
    let forged = Event::AskRequested {
        track_id: boot.track_id.clone(),
        questions: vec![AskQuestion {
            title: "Re-run?".into(),
            options: vec!["Yes".into(), "No".into()],
        }],
        delivery: AskDelivery::Wake,
        action: Some(AskAction::ForgeAction {
            idempotency_key: "other".into(),
            payload_hash: HASH.into(),
        }),
        source_item_id: None,
    };
    let actor = ActorId::AiPlanner(boot.planner_card_id.clone());
    let (_, ids) = calm_server::db::write_with_actor_events_typed::<(), _>(
        boot.repo.as_ref(),
        None,
        &boot.events,
        &boot.ctx.write,
        move |_tx| {
            Box::pin(async move {
                let scope = calm_server::event::EventScope::Track {
                    track: scope_track,
                    area: scope_area,
                };
                Ok(((), vec![(actor, scope, forged)]))
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(
        answer(&boot, ids[0], json!([{ "option": 0 }])).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(rows(&boot, "ask.answered").await.is_empty());
}

#[tokio::test]
async fn forge_approval_click_has_no_side_effect_events() {
    for (answers, approval) in [
        (json!([{ "option": 0 }]), ForgeApproval::Approved),
        (
            json!([{ "option": 1 }]),
            ForgeApproval::Declined { ask_id: 0 },
        ),
        (
            json!([{ "text": "Approve" }]),
            ForgeApproval::Declined { ask_id: 0 },
        ),
    ] {
        let boot = boot().await;
        let id = raise(&boot).await;
        let before = boot
            .repo
            .events_since(0, i64::MAX)
            .await
            .unwrap()
            .last()
            .unwrap()
            .0;
        assert_eq!(
            answer(&boot, id, answers.clone()).await.0,
            StatusCode::NO_CONTENT
        );
        let written: Vec<String> = boot
            .repo
            .events_since(before, i64::MAX)
            .await
            .unwrap()
            .into_iter()
            .map(|(_, _, _, event)| event.kind_tag().to_string())
            .collect();
        assert_eq!(written, ["ask.answered"], "{answers}: the answer alone");
        let expected = match approval {
            ForgeApproval::Declined { .. } => ForgeApproval::Declined { ask_id: id },
            other => other,
        };
        assert_eq!(
            gate(&boot, &boot.planner_card_id).await.unwrap(),
            Some(expected),
            "{answers}: only a click on Approve grants"
        );
        assert!(activity_items(&boot).await.is_empty());
    }
}
