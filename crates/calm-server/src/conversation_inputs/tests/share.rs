//! Live issuer + real delegation + production acceptance/claim storage.
use super::*;
use calm_types::conversation::{ConversationExecutionProvenance, MessageAuthor};
use calm_types::share_links::ShareScope;

async fn delegation(fx: &Fixture, area_scope: bool) -> ShareInputSource {
    let token = fx.session(LEGACY_OWNER_USER_ID, "owner");
    let human = super::execution::source(&fx.auth, &token).await;
    let scope = crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let card = fx.input.card.clone();
        move |tx| {
            Box::pin(async move {
                let area = conversation::conversation_area_tx(tx, &card).await?;
                let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                    .bind(&card)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(if area_scope {
                    ShareScope::Area(area.id.to_string())
                } else {
                    ShareScope::Track(track)
                })
            })
        }
    })
    .await
    .unwrap();
    let link = crate::share_access::create(fx.input.repo.as_ref(), human, scope)
        .await
        .unwrap();
    let auth = crate::auth::ValidatedApplicationAuth::from_state(fx.auth.clone()).unwrap();
    ShareInputSource::new(auth.execution, link.link.delegation)
}

async fn accept(fx: &Fixture, share: &ShareInputSource) -> MessageRevisionRef {
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let (card, share) = (fx.input.card.clone(), share.clone());
        move |tx| {
            Box::pin(async move {
                share.authorize_tx(tx, &card).await?;
                let mut carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                let accepted = conversation::accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: share.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "guest words".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?;
                let calm_types::conversation::AcceptedInputRef::Human { message } =
                    accepted.input_ref
                else {
                    panic!("guest is a message")
                };
                Ok(message)
            })
        }
    })
    .await
    .unwrap()
}

async fn claim(fx: &Fixture, share: &ShareInputSource) -> Result<ClaimedSubmission> {
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let (card, share) = (fx.input.card.clone(), share.clone());
        move |tx| {
            Box::pin(async move {
                let mut carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                let source = InputSource::Share(share);
                let plan = super::super::pending_selection::plan_tx(tx, &source, &carrier).await?;
                claim_initial_tx(tx, source, &mut carrier, plan).await
            })
        }
    })
    .await
}

#[tokio::test]
async fn accepted_share_executes_after_access_revocation_with_lossless_provenance() {
    let fx = Fixture::new(false).await;
    let share = delegation(&fx, false).await;
    let reference = accept(&fx, &share).await;
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let id = share.delegation().id.clone();
        move |tx| {
            Box::pin(async move {
                crate::db::sqlite::share_links::revoke_tx(tx, &id, crate::model::now_ms())
                    .await
                    .map(drop)
                    .map_err(Into::into)
            })
        }
    })
    .await
    .unwrap();
    let claimed = claim(&fx, &share).await.unwrap();
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let (card, delegation) = (fx.input.card.clone(),share.delegation().clone());
        move |tx| Box::pin(async move {
            let identity = conversation::submission_identity_tx(tx,&claimed.submission_id).await?;
            assert!(matches!(identity.execution,ConversationExecutionProvenance::ShareDelegation { delegation: ref actual,.. } if *actual==delegation));
            let plan = conversation::submission_plan_tx(tx,&claimed.submission_id).await?;
            assert_eq!(plan.plan.input_refs().unwrap(),vec![calm_types::conversation::AcceptedInputRef::Human { message:reference }]);
            let hydrated = crate::harness::input_dispatch::hydrate::hydrate(tx,&card,&plan.plan).await?;
            assert!(hydrated.iter().any(|item| matches!(item, provider::InputItem::Text { text } if text.contains("guest words"))));
            Ok(())
        })
    }).await.unwrap();
}

#[tokio::test]
async fn share_execution_holds_after_current_area_ownership_loss() {
    let fx = Fixture::new(false).await;
    let share = delegation(&fx, false).await;
    accept(&fx, &share).await;
    super::execution::transfer_area_to_alice(&fx).await;
    assert!(matches!(
        claim(&fx, &share).await,
        Err(CalmError::Forbidden(_))
    ));
}

#[tokio::test]
async fn share_execution_holds_after_scope_loss_and_old_area_deletion_without_losing_history() {
    let fx = Fixture::new(false).await;
    let share = delegation(&fx, true).await;
    let message = accept(&fx, &share).await;
    let claimed = claim(&fx, &share).await.unwrap();
    let next = fx
        .input
        .repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "new home".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let (card,area) = (fx.input.card.clone(),next.id.to_string());
        move |tx| Box::pin(async move {
            sqlx::query("UPDATE tracks SET area_id=?1 WHERE id=(SELECT track_id FROM cards WHERE id=?2)").bind(area).bind(card).execute(&mut **tx).await?;
            Ok(())
        })
    }).await.unwrap();
    assert!(matches!(
        revalidate(&fx, &share, &claimed.submission_id).await,
        Err(CalmError::Forbidden(_))
    ));
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let area = match &share.delegation().scope {
            ShareScope::Area(id) => id.clone(),
            _ => panic!("area scope"),
        };
        move |tx| {
            Box::pin(async move {
                sqlx::query("DELETE FROM areas WHERE id=?1")
                    .bind(area)
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            })
        }
    })
    .await
    .unwrap();
    assert!(
        revalidate(&fx, &share, &claimed.submission_id)
            .await
            .is_err()
    );
    let expected = share.delegation().clone();
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let identity = conversation::submission_identity_tx(tx,&claimed.submission_id).await?;
            assert!(matches!(identity.execution, ConversationExecutionProvenance::ShareDelegation { delegation, .. } if delegation == expected));
            assert!(matches!(
                conversation::revision_tx(tx, &message).await?.payload,
                calm_types::conversation::PrivateHistoryPayload::Present { .. }
            ));
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn share_source_stops_at_owner_and_other_guest_prefixes() {
    let fx = Fixture::new(false).await;
    let a = delegation(&fx, false).await;
    let b = delegation(&fx, false).await;
    accept(&fx, &a).await;
    accept(&fx, &a).await;
    accept(&fx, &b).await;
    accept(&fx, &a).await;
    let token = fx.session(LEGACY_OWNER_USER_ID, "owner");
    assert_eq!(fx.send(Some(&token)).await.status(), StatusCode::OK);
    let human = super::execution::source(&fx.auth, &token).await;
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let card = fx.input.card.clone();
        move |tx| {
            Box::pin(async move {
                let carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                let source = InputSource::Share(a);
                let plan = super::super::pending_selection::plan_tx(tx, &source, &carrier).await?;
                assert_eq!(plan.input_refs().unwrap().len(), 2);
                assert!(
                    authorize_execution_tx(tx, human, &carrier, &plan)
                        .await
                        .is_err()
                );
                let other = InputSource::Share(b);
                assert!(
                    authorize_execution_tx(tx, other, &carrier, &plan)
                        .await
                        .is_err()
                );
                Ok(())
            })
        }
    })
    .await
    .unwrap();
}

async fn revalidate(
    fx: &Fixture,
    share: &ShareInputSource,
    id: &calm_types::conversation::InputSubmissionId,
) -> Result<()> {
    crate::db::write_in_tx_typed(fx.input.repo.as_ref(), {
        let (card, source, id) = (
            fx.input.card.clone(),
            InputSource::Share(share.clone()),
            id.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                let stored = conversation::submission_plan_tx(tx, &id).await?;
                authorize_execution_tx(tx, source, &carrier, &stored.plan)
                    .await
                    .map(drop)
            })
        }
    })
    .await
}

#[tokio::test]
async fn accepted_share_holds_when_its_real_issuer_disappears() {
    let fx = Fixture::new(false).await;
    let share = delegation(&fx, false).await;
    accept(&fx, &share).await;
    let auth = crate::auth::ValidatedApplicationAuth::from_state(AuthState::new(AuthConfig {
        username: Some("owner".into()),
        password: Some("password".into()),
        dev_autologin: false,
        display_name: "Owner".into(),
    }))
    .unwrap();
    let recovered = ShareInputSource::new(auth.execution, share.delegation().clone());
    assert!(recovered.check_current().is_ok());
    drop(auth.state);
    assert!(matches!(
        claim(&fx, &recovered).await,
        Err(CalmError::Unauthorized)
    ));
}
