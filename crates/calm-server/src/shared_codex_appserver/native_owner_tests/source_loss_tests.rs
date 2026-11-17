//! Local reader termination must reach progress even when content forwarding cannot.
use super::*;
use futures_util::FutureExt;
use tokio::sync::Notify;

async fn hint(signal: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .unwrap();
}
fn no_hint(signal: &Notify) {
    assert!(signal.notified().now_or_never().is_none());
}
async fn join_forwarder(mut forwarder: JoinHandle<()>) {
    let joined = tokio::time::timeout(Duration::from_secs(5), &mut forwarder).await;
    if joined.is_err() {
        forwarder.abort();
        let _ = forwarder.await;
    }
    joined.unwrap().unwrap();
}

// The lock stays outside the caught body so every failure releases it before joining.
async fn blocked_consumers(unregister: bool, register_late: bool) {
    let daemon = daemon().await;
    let (client, mut peer, forwarder) = connect_with_reader(&daemon).await;
    let signals: Vec<_> = (0..3).map(|_| Arc::new(Notify::new())).collect();
    let old = daemon.watch_native_source_loss(&signals[0]);
    let current = unregister.then(|| daemon.watch_native_source_loss(&signals[0]));
    let _other = daemon.watch_native_source_loss(&signals[1]);
    let cancelled = daemon.watch_native_source_loss(&signals[2]);
    drop(cancelled);
    if unregister {
        drop(old);
    } // The second token for this same signal must survive.
    let attribution = hold_attribution(&daemon).await;
    let result = std::panic::AssertUnwindSafe(async {
        no_hint(&signals[0]);
        send(
            &mut peer,
            json!({"method":"thread/started","params":{"thread":{"id":"delayed"}}}),
        )
        .await;
        attribution_blocked(&daemon).await;
        peer.close(None).await.unwrap();
        reader_finished(&client).await;
        let late_signal = Arc::new(Notify::new());
        let _late = register_late.then(|| daemon.watch_native_source_loss(&late_signal));
        if register_late {
            assert!(late_signal.notified().now_or_never().is_some());
        } else {
            hint(&signals[0]).await;
            hint(&signals[1]).await;
        }
        no_hint(&signals[2]);
        assert!(
            !daemon
                .native_turns
                .is_current_source(&Arc::downgrade(&client))
        );
        assert!(!forwarder.is_finished());
    })
    .catch_unwind()
    .await;
    drop(attribution);
    drop(peer);
    // Do not let a cleanup error hide the assertion that originally failed.
    let joined = std::panic::AssertUnwindSafe(join_forwarder(forwarder))
        .catch_unwind()
        .await;
    drop(current);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if let Err(panic) = joined {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn source_loss_wakes_while_attribution_blocks_the_forwarder() {
    blocked_consumers(false, false).await;
}
#[tokio::test]
async fn source_loss_unregistering_one_consumer_keeps_the_others() {
    blocked_consumers(true, false).await;
}
#[tokio::test]
async fn source_loss_registered_after_eof_signals_at_registration() {
    blocked_consumers(false, true).await;
}

#[tokio::test]
async fn source_loss_wakes_while_a_full_harness_queue_blocks_the_forwarder() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let daemon = daemon().await;
    let (client, mut peer, forwarder) = connect_with_reader(&daemon).await;
    let receiver = daemon.subscribe_harness_notifications();
    let signal = Arc::new(Notify::new());
    let _watch = daemon.watch_native_source_loss(&signal);
    let saturated = PausePoint::default();
    let key = daemon.sock_path().to_string_lossy().into_owned();
    install_pause_for_test("codex-notification-backpressure", &key, saturated.clone());
    let result = std::panic::AssertUnwindSafe(async {
        no_hint(&signal);
        for index in 0..17 {
            send(&mut peer, json!({"method":"item/completed","params":{
                "threadId":"thread","turnId":"old","item":{"id":format!("old-{index}"),"type":"agentMessage","text":"old"}
            }})).await;
        }
        hint(&saturated.entered).await;
        peer.close(None).await.unwrap();
        reader_finished(&client).await;
        hint(&signal).await;
        assert!(!daemon.native_turns.is_current_source(&Arc::downgrade(&client)));
        assert!(!forwarder.is_finished());
    }).catch_unwind().await;
    if let Some(armed) = calm_truth::test_seam::take("codex-notification-backpressure", &key) {
        armed.release.notify_one();
    }
    drop(receiver);
    saturated.release.notify_one();
    drop(peer);
    let joined = std::panic::AssertUnwindSafe(join_forwarder(forwarder))
        .catch_unwind()
        .await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if let Err(panic) = joined {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn source_loss_follows_the_current_source_across_replacement() {
    let daemon = daemon().await;
    let signal = Arc::new(Notify::new());
    let _watch = daemon.watch_native_source_loss(&signal);
    assert!(signal.notified().now_or_never().is_some()); // No source at registration.
    let (_old, old_peer, old_forwarder) = connect_with_reader(&daemon).await;
    hint(&signal).await;
    let (new, new_peer, new_forwarder) = connect_with_reader(&daemon).await;
    hint(&signal).await;
    no_hint(&signal);
    drop(old_peer);
    let result = std::panic::AssertUnwindSafe(async {
        join_forwarder(old_forwarder).await;
        no_hint(&signal);
        assert!(daemon.native_turns.is_current_source(&Arc::downgrade(&new)));
        drop(new_peer);
        hint(&signal).await;
    })
    .catch_unwind()
    .await;
    let joined = std::panic::AssertUnwindSafe(join_forwarder(new_forwarder))
        .catch_unwind()
        .await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if let Err(panic) = joined {
        std::panic::resume_unwind(panic);
    }
}
