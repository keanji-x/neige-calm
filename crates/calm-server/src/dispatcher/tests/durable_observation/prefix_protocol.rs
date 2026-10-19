use super::*;

#[tokio::test]
async fn durable_observation_ordinary_successor_repairs_failed_prefix_without_redelivery() {
    let fx = fixture().await;
    let a = fx.envelope("failure").await;
    let a_id = a.id;
    let b = fx.envelope("success").await;
    let b_id = b.id;
    sqlx::query(concat!(
        "CREATE TRIGGER prefix_failure BEFORE UPDATE OF handle_state_json ON worker_sessions ",
        "BEGIN SELECT RAISE(ABORT, 'prefix failure'); END",
    ))
    .execute(fx.repo.db().writer_pool())
    .await
    .unwrap();
    fx.inner.clone().handle_envelope(a.clone()).await;
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), 0);
    assert_eq!(fx.handle.snapshot().await.push_watermark, 0);
    assert!(fx.handle.snapshot().await.pending_entries().is_empty());
    assert_eq!(fx.stored().await.push_watermark, 0);
    assert!(fx.stored().await.pending_entries().is_empty());
    sqlx::query("DROP TRIGGER prefix_failure")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    // Only B is delivered. A must come from the production prefix lookup, never a test replay.
    fx.inner.clone().handle_envelope(b.clone()).await;
    let stored = fx.stored().await;
    let ids: Vec<_> = stored
        .pending_entries()
        .iter()
        .map(|e| e.envelope_id())
        .collect();
    assert_eq!(
        ids,
        vec![Some(a_id), Some(b_id)],
        "SQL queue at watermark {} / cursor {}",
        stored.push_watermark,
        fx.inner.push_cursor.get(&fx.card.id)
    );
    assert_eq!(stored.push_watermark, b_id);
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), b_id);
    fx.inner
        .harness
        .shutdown_and_remove(&fx.runtime)
        .await
        .unwrap();
    let reopened = Arc::new(SqlxRepo::open(&fx.url).await.unwrap());
    let runtime = reopened
        .session_projection_by_id(&fx.runtime)
        .await
        .unwrap()
        .unwrap();
    let on_disk = HarnessSnapshot::from_value_strict(runtime.handle_state_json.clone().unwrap());
    assert_eq!(on_disk.pending_entries(), stored.pending_entries());
    assert_eq!(on_disk.push_watermark, b_id);
    let daemon = SharedCodexAppServer::new_stub_with_pending(reopened.clone(), None);
    let recovered = crate::harness::spawn_recovered_harness(
        reopened.clone(),
        EventBus::new(),
        fx.roles.clone(),
        TrackAreaCache::new(),
        daemon.clone(),
        daemon.thread_seals().clone(),
        &crate::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(reopened),
        &fx.inner.harness,
        &crate::harness::new_track_delete_locks(),
        runtime,
        crate::harness::ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("real recovery installs the durable prefix");
    recovered.pause_issuance_for_dev();
    assert_eq!(
        recovered.snapshot().await.pending_entries(),
        stored.pending_entries()
    );
    assert_eq!(recovered.snapshot().await.push_watermark, b_id);
    fx.inner.push_cursor.remove(&fx.card.id);
    fx.inner.clone().handle_envelope(b).await;
    fx.inner.clone().handle_envelope(a).await;
    assert_eq!(
        fx.stored().await.pending_entries(),
        stored.pending_entries()
    );
    assert_eq!(fx.stored().await.push_watermark, b_id);
    fx.inner
        .harness
        .shutdown_and_remove(&fx.runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn durable_observation_failed_prefix_blocks_successors_and_bounds_catch_up() {
    let fx = fixture().await;
    let a = fx.envelope("failure").await;
    // A global event ID belongs to another scope. Prefixes are not id+1 sequences.
    let mut tx = crate::db::sqlite::begin_immediate_tx(fx.repo.db())
        .await
        .unwrap();
    append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::System,
        None,
        &a.event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let b = fx.envelope("success").await;
    let c = fx.envelope("later").await;
    assert!(b.id > a.id + 1);
    // Only A's snapshot write fails; an implementation that skips it could successfully ACK B.
    sqlx::query(&format!(
        "CREATE TRIGGER prefix_failure BEFORE UPDATE OF handle_state_json ON worker_sessions \
         WHEN json_extract(NEW.handle_state_json, '$.push_watermark') = {} \
         BEGIN SELECT RAISE(ABORT, 'prefix failure'); END",
        a.id
    ))
    .execute(fx.repo.db().writer_pool())
    .await
    .unwrap();
    fx.inner.clone().handle_envelope(a.clone()).await;
    fx.inner.clone().handle_envelope(b.clone()).await;
    assert_eq!(fx.stored().await.push_watermark, 0);
    assert!(fx.stored().await.pending_entries().is_empty());
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), 0);
    sqlx::query("DROP TRIGGER prefix_failure")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    // A hot cache from an earlier owner/reset must not override the lower current SQL floor.
    fx.inner.push_cursor.bump(fx.card.id.clone(), c.id);
    fx.inner.clone().handle_envelope(b.clone()).await;
    let stored = fx.stored().await;
    assert_eq!(stored.push_watermark, b.id);
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), b.id);
    assert_eq!(
        stored
            .pending_entries()
            .iter()
            .filter_map(|e| e.envelope_id())
            .collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
    // Cold cursor and old/duplicate delivery cannot remove pending <= watermark or duplicate them.
    fx.inner.push_cursor.remove(&fx.card.id);
    fx.inner.clone().handle_envelope(a).await;
    fx.inner.clone().handle_envelope(b).await;
    assert_eq!(
        fx.stored().await.pending_entries(),
        stored.pending_entries()
    );
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), stored.push_watermark);
    // A committed later event stays out of the bounded B prefix until it actually arrives.
    fx.inner.clone().handle_envelope(c.clone()).await;
    assert_eq!(fx.stored().await.push_watermark, c.id);
    assert_eq!(fx.stored().await.pending_entries().len(), 3);
    fx.inner
        .harness
        .shutdown_and_remove(&fx.runtime)
        .await
        .unwrap();
}

#[tokio::test]
async fn durable_observation_mapping_failure_pins_ordinary_successor_until_resolved() {
    let fx = fixture().await;
    let a = fx
        .persist(Event::TaskCompleted {
            idempotency_key: "legacy-task".into(),
            result: serde_json::json!({"message":"A"}),
            artifacts: vec![],
            agent_message: None,
        })
        .await;
    let b = fx.envelope("success").await;
    // Real SQL failure in the existing completion enrichment, before any queue staging.
    sqlx::query("ALTER TABLE tasks RENAME TO prefix_hidden_tasks")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    fx.inner.clone().handle_envelope(a.clone()).await;
    fx.inner.clone().handle_envelope(b.clone()).await;
    assert_eq!(fx.stored().await.push_watermark, 0);
    assert!(fx.stored().await.pending_entries().is_empty());
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), 0);
    sqlx::query("ALTER TABLE prefix_hidden_tasks RENAME TO tasks")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    fx.inner.clone().handle_envelope(b.clone()).await;
    assert_eq!(fx.stored().await.push_watermark, b.id);
    assert_eq!(
        fx.stored()
            .await
            .pending_entries()
            .iter()
            .filter_map(|e| e.envelope_id())
            .collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
    fx.inner
        .harness
        .shutdown_and_remove(&fx.runtime)
        .await
        .unwrap();
}
