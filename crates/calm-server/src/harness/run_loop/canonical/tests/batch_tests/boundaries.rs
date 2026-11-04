//! A current source may consume only its contiguous qualified Human prefix.
use super::fixture::*;
use super::*;

#[tokio::test]
async fn canonical_batch_erased_tail_stops_prefix_without_skipping_later_human() {
    let fx = Fixture::new().await;
    let second = content_tests::accept(&fx, "second qualified human").await;
    let unavailable = content_tests::accept(&fx, "erased Human boundary").await;
    let later = content_tests::accept(&fx, "qualified but behind erased Human").await;
    erase(&fx, &unavailable).await;
    let before = fx.frame().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    observe(&fx).await;
    let call = content_tests::next_start(&mut peer).await;
    let crossing = fx.frame().await;
    let refs = stored_plan(&fx, &crossing).await.input_refs().unwrap();
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":"malformed","message":"hold batch"}}),
    )
    .await;
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
    assert_eq!(
        refs,
        vec![
            AcceptedInputRef::Human {
                message: fx.message.clone()
            },
            AcceptedInputRef::Human { message: second },
        ]
    );
    assert_eq!(
        crossing.canonical_inputs.pending,
        before.canonical_inputs.pending[2..]
    );
    assert_eq!(
        crossing.canonical_inputs.pending[0]
            .accepted_input_ref()
            .unwrap(),
        AcceptedInputRef::Human {
            message: unavailable
        }
    );
    assert_eq!(
        crossing.canonical_inputs.pending[1].identity(),
        later.into()
    );
    let text = call["params"]["input"][0]["text"].as_str().unwrap();
    assert!(!text.contains("erased Human boundary"));
    assert!(!text.contains("qualified but behind erased Human"));
}

#[tokio::test]
async fn canonical_batch_empty_pending_does_not_read_or_start() {
    let fx = Fixture::new().await;
    let (_, _, _, handoff) = crate::conversation_inputs::pending_controls::mutate(
        fx.original.repo.as_ref(),
        fx.registry.clone(),
        fx.source.clone(),
        crate::conversation_inputs::pending_controls::PendingHumanMutation {
            card: fx.harness.inner.card_id.to_string(),
            worker: fx.harness.inner.worker_session_id.clone(),
            expected: fx.message.clone(),
            change: crate::db::sqlite::conversation::PendingHumanChange::Withdraw,
        },
    )
    .await
    .unwrap();
    drop(handoff);
    let (_client, mut peer) = connect(&fx.daemon).await;
    observe(&fx).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    let idle = fx.frame().await;
    assert!(idle.canonical_inputs.pending.is_empty());
    assert!(idle.canonical_inputs.active.is_none());
    assert!(idle.canonical_inputs.attempts.is_empty());
    fx.harness.shutdown().await.unwrap();
}
