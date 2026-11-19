//! Detached Close consumes only the original durable provider receipt.
use super::*;

#[tokio::test]
async fn close_p1_detached_acp_completed_receipt_settles_without_new_prompt() {
    detached_receipt(AgentProvider::OpenCode, "reply", false, false).await;
}

#[tokio::test]
async fn close_p1_detached_nonresident_grok_completed_receipt_settles_without_new_prompt() {
    detached_receipt(AgentProvider::Grok, "reply", false, false).await;
}

#[tokio::test]
async fn close_p1_detached_acp_unknown_receipt_keeps_the_original_claim() {
    detached_receipt(AgentProvider::OpenCode, "lost", false, false).await;
}

#[tokio::test]
async fn close_p1_detached_acp_foreign_receipt_keeps_the_original_claim() {
    detached_receipt(AgentProvider::OpenCode, "reply", true, false).await;
}

#[tokio::test]
async fn close_p1_detached_acp_predecessor_cannot_observe_its_successors_execution() {
    detached_receipt(AgentProvider::OpenCode, "reply", false, true).await;
}

async fn detached_receipt(provider: AgentProvider, scenario: &str, foreign: bool, replaced: bool) {
    let fx = Fixture::with_provider(provider, scenario, true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.session.shutdown().await.unwrap();
    let acp = Arc::new(AcpPlannerHost::unconfigured_scratch().unwrap());
    acp.completion.bind(fx.owner.repo.clone());
    acp.execution.bind(fx.dir._execution.engine.clone());
    crate::acp_planner::config::boot_with_empty_marker_scan_for_test(fx.owner.repo.as_ref(), &acp)
        .await
        .unwrap();
    if foreign {
        fx.owner.repo.db().writer().execute(
            sqlx::query("UPDATE acp_submissions SET native_session_id='foreign-native' WHERE worker_session_id=?1")
                .bind(&fx.owner.worker),
        ).await.unwrap();
    }
    let before = fx.prompts();
    let track = fx.owner.track.clone();
    let worker = fx.owner.worker.clone();
    let runtime = crate::db::write_in_tx_typed(fx.owner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::track_close::close_subtree_tx(
                tx,
                &track.into(),
                crate::ids::ActorId::User,
                None,
            )
            .await?;
            Ok(crate::db::sqlite::session_projection_by_id_tx(tx, &worker)
                .await?
                .unwrap())
        })
    })
    .await
    .unwrap();
    let claude =
        Arc::new(crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap());
    let factory = crate::harness::backend::factory::PlannerFactory::new(
        std::sync::Weak::new(),
        &claude,
        &acp,
    );
    let cleanup = sqlx::query_as::<_, crate::track_close::PlannerCleanup>(
        "SELECT worker_session_id,card_id,track_id,requested_at_ms FROM track_close_cleanups WHERE worker_session_id=?1",
    ).bind(&fx.owner.worker).fetch_one(fx.owner.repo.pool()).await.unwrap();
    let successor = if replaced {
        Some(
            crate::harness::backend::dispatch_fixture::supersede_planner(
                fx.owner.repo.as_ref(),
                &fx.owner.card,
                &fx.owner.worker,
                fx.provider.clone(),
            )
            .await,
        )
    } else {
        None
    };
    let stopped = factory.shutdown_closed_session(&runtime, None).await;
    let settled = match stopped {
        Ok(()) => {
            crate::harness::backend::factory::PlannerFactory::settle_closed_session(
                fx.owner.repo.as_ref(),
                &runtime,
                None,
                &cleanup,
                None,
            )
            .await
        }
        Err(error) => Err(error),
    };
    assert_eq!(fx.prompts(), before, "Close must not replay a prompt");
    assert_eq!(
        settled.is_ok(),
        scenario == "reply" && !foreign,
        "{settled:?}"
    );
    let card = fx.owner.card.clone();
    let worker = fx.owner.worker.clone();
    let expected_settled = scenario == "reply" && !foreign;
    crate::db::write_in_tx_typed(fx.owner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            if let Some(successor) = successor {
                let current = crate::db::sqlite::session_projection_by_id_tx(tx, &successor.id)
                    .await?
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(current).unwrap(),
                    serde_json::to_value(successor).unwrap()
                );
            } else {
                let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
                assert_eq!(carrier.checkpoint().active.is_none(), expected_settled);
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}
