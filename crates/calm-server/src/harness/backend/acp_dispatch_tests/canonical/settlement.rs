//! END must settle receipt items even when the live event consumer is behind.
use super::*;

async fn presentations(fx: &Fixture) -> Vec<(i64, i64, String, String)> {
    sqlx::query_as(
        "SELECT p.sequence,h.id,h.method,h.params FROM conversation_presentations p
        JOIN harness_items h ON h.id=p.native_item_row_id WHERE p.card_id=?1 ORDER BY p.sequence",
    )
    .bind(&fx.owner.card)
    .fetch_all(fx.owner.repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn acp_assistant_end_settles_native_items_before_idle_and_late_event_replay() {
    let fx = Fixture::new("reply").await;
    // Subscribe without consuming: no harness observer can persist these events.
    let mut events = fx.session.subscribe_events();
    fx.dispatch().await.unwrap();
    let end = fx.wait_end().await;
    assert!(
        serde_json::from_value::<crate::harness::snapshot::CanonicalHarnessSnapshot>(end.snapshot)
            .unwrap()
            .idle_for_lifecycle()
    );
    let before = presentations(&fx).await;
    assert_eq!(
        before.len(),
        3,
        "END must project both replies and the tool"
    );
    assert!(before.iter().all(|row| row.2 == "item/completed"));
    assert!(before.iter().any(|row| row.3.contains("canonical human")));
    // Replay the real queued events through the same Truth entry as the live observer.
    while let Ok(event) = events.try_recv() {
        if let provider::events::PlannerEventKind::Item { phase, params, .. } = event.kind {
            let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
            let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                .await
                .unwrap();
            observe_native_item_tx(
                &mut tx,
                &carrier,
                &fx.owner.thread,
                params["turnId"].as_str(),
                phase.method(),
                &params,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
    }
    assert_eq!(
        presentations(&fx).await,
        before,
        "late live frames cannot change the settled snapshot"
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_assistant_end_retries_item_persistence_without_reprompting() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    sqlx::query(
        "CREATE TRIGGER refuse_settlement_item BEFORE INSERT ON harness_items
        WHEN NEW.method='item/completed' BEGIN SELECT RAISE(ABORT,'item persistence failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match crate::harness::input_dispatch::complete_once(
                fx.owner.repo.as_ref(),
                &fx.backend,
                &fx.owner.card,
                &fx.owner.worker,
            )
            .await
            {
                Err(error) => break error,
                Ok(Some(_)) => panic!("END released before item persistence"),
                Ok(None) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("finished driver must attempt item persistence");
    assert!(
        error.to_string().contains("item persistence failed"),
        "{error}"
    );
    assert!(presentations(&fx).await.is_empty());
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert!(
        carrier.checkpoint().active.is_some(),
        "failed projection cannot release END"
    );
    tx.commit().await.unwrap();
    sqlx::query("DROP TRIGGER refuse_settlement_item")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    fx.wait_end().await;
    assert_eq!(presentations(&fx).await.len(), 3);
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}
