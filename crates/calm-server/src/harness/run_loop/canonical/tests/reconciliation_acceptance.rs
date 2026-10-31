//! Complete-loop admission and recovery, using the real durable input/source owners.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use crate::test_seams::{PausePoint, install_pause_for_test};
use futures::FutureExt;

async fn completed_idle(fx: &Fixture, peer: &mut Peer) -> PausePoint {
    super::control_tests::running(fx, peer).await;
    let original = fx.frame().await;
    let idle = PausePoint::default();
    install_pause_for_test(
        "canonical-idle-wait",
        fx.harness.inner.card_id.as_str(),
        idle.clone(),
    );
    notify_thread(
        &fx.daemon,
        peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), idle.entered.notified())
        .await
        .unwrap();
    idle
}

#[tokio::test]
async fn unchanged_idle_full_reconciliation_does_not_need_the_writer() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let idle = completed_idle(&fx, &mut peer).await;
    let card = fx.harness.inner.card_id.as_str();
    let observed = PausePoint::default();
    install_pause_for_test("canonical-idle-wait", card, observed.clone());
    let writer = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    idle.release.notify_one();
    fx.harness.request_progress();
    let advanced = tokio::time::timeout(Duration::from_secs(1), observed.entered.notified()).await;
    writer.rollback().await.unwrap();
    observed.release.notify_one();
    assert!(
        advanced.is_ok(),
        "the full unchanged idle pass must not queue behind a writer"
    );
    assert!(fx.frame().await.canonical_inputs.active.is_none());
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_source_hint_is_reconciled_from_real_committed_facts() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let idle = completed_idle(&fx, &mut peer).await;
    let message =
        super::content_tests::accept(&fx, "recover the committed input after a lost hint").await;
    let inner = &fx.harness.inner;
    let sources = fx
        .registry
        .canonical_sources(&inner.worker_session_id, &inner.track_id);
    sources.observe(message.into(), fx.source.clone().into());
    // The real producer emitted its hint; discard it while the actor is held
    // before sleep. Its body and exact live source remain with their owners.
    assert!(sources.changed().now_or_never().is_some());
    let scheduled = PausePoint::default();
    install_pause_for_test(
        "canonical-recovery-probe-ready",
        inner.card_id.as_str(),
        scheduled.clone(),
    );
    idle.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), scheduled.entered.notified())
        .await
        .unwrap();
    // The real absolute deadline exists before advancing the clock. Keep the
    // actor at this barrier so no assumption about yield counts is necessary.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    scheduled.release.notify_one();
    // SQL and native I/O use real workers; resume before awaiting either.
    tokio::time::resume();
    let call = tokio::time::timeout(
        Duration::from_secs(3),
        super::content_tests::next_start(&mut peer),
    )
    .await
    .expect("a lost hint must not strand an already committed input");
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("recover the committed input after a lost hint")
    );
    let thread = call["params"]["threadId"].as_str().unwrap().to_owned();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"reconciled-turn"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"reconciled-turn"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    super::content_tests::complete(&fx, &mut peer, &thread, "reconciled-turn").await;
    assert!(fx.frame().await.canonical_inputs.pending.is_empty());
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}
