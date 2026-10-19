use super::*;

async fn fence_case(case: u8, crossing: bool) {
    let fx = Fixture::with_claim(crossing).await;
    let before = fx.checkpoint().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let area = conversation_area_tx(&mut tx, &fx.card).await.unwrap();
    if case != 2 {
        crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
            .await
            .unwrap();
        if case == 1 {
            sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)")
                    .bind(area.id.as_str()).execute(&mut **tx).await.unwrap();
        }
    } else {
        // Keep the existing owner membership to model a corrupted System Area.
        sqlx::query("UPDATE areas SET kind='user' WHERE kind='system'")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("UPDATE areas SET kind='system' WHERE id=?1")
            .bind(area.id.as_str())
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    if !crossing {
        let mut carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
            .await
            .unwrap();
        let plan = SubmissionRenderPlan::V1 {
            segments: vec![SubmissionRenderSegment::Input {
                input_position: 0,
                input: SubmissionRenderInput::Human {
                    message: fx.message.clone(),
                    author: FrozenHumanAttribution {
                        registered_user_id: LEGACY_OWNER_USER_ID.into(),
                        display_name: "Owner".into(),
                    },
                },
            }],
        };
        let claim =
            crate::conversation_inputs::claim_initial_tx(&mut tx, &fx.source, &mut carrier, plan)
                .await;
        assert!(
            matches!(claim, Err(CalmError::Forbidden(_))),
            "claim case {case}: {claim:?}"
        );
    }
    tx.commit().await.unwrap();
    if crossing {
        assert!(
            matches!(fx.dispatch().await, Err(CalmError::Forbidden(_))),
            "crossing case {case}"
        );
    }
    assert_eq!(fx.checkpoint().await, before);
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn human_private_fence_other_registered_user_claim() {
    fence_case(0, false).await;
}

#[tokio::test]
async fn human_private_fence_other_registered_user_crossing() {
    fence_case(0, true).await;
}

#[tokio::test]
async fn human_private_fence_extra_user_area_member_claim() {
    fence_case(1, false).await;
}

#[tokio::test]
async fn human_private_fence_extra_user_area_member_crossing() {
    fence_case(1, true).await;
}

#[tokio::test]
async fn human_private_fence_nonempty_system_area_claim() {
    fence_case(2, false).await;
}

#[tokio::test]
async fn human_private_fence_nonempty_system_area_crossing() {
    fence_case(2, true).await;
}
