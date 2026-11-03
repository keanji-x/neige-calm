//! Lossless handoff to the harnesses that persist native content.
//!
//! Unlike diagnostic broadcast, a full queue backpressures the forwarder. Reserve
//! outside the native owner lock; publish synchronously under its source fence.
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::codex_appserver::Notification;

pub(super) struct HarnessNotifications {
    capacity: usize,
    subscribers: Mutex<Vec<mpsc::Sender<Arc<Notification>>>>,
}

impl HarnessNotifications {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            subscribers: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn subscribe(&self) -> mpsc::Receiver<Arc<Notification>> {
        let (sender, receiver) = mpsc::channel(self.capacity);
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("harness notification subscribers");
        subscribers.retain(|sender| !sender.is_closed());
        subscribers.push(sender);
        receiver
    }

    fn subscribers(&self) -> Vec<mpsc::Sender<Arc<Notification>>> {
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("harness notification subscribers");
        subscribers.retain(|sender| !sender.is_closed());
        subscribers.clone()
    }

    pub(super) async fn reserve(&self, owner: &str) -> Publication {
        #[cfg(not(feature = "fixtures"))]
        let _ = owner;
        let mut permits = Vec::new();
        for sender in self.subscribers() {
            match sender.clone().try_reserve_owned() {
                Ok(permit) => permits.push(permit),
                Err(mpsc::error::TrySendError::Closed(_)) => {}
                Err(mpsc::error::TrySendError::Full(sender)) => {
                    #[cfg(feature = "fixtures")]
                    crate::test_seams::pause_point("codex-notification-backpressure", owner).await;
                    // Dropping the receiver cancels this wait. Holding no registry,
                    // database or source lock here keeps shutdown/replacement free.
                    if let Ok(permit) = sender.reserve_owned().await {
                        permits.push(permit);
                    }
                }
            }
        }
        Publication(permits)
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn publish_for_test(&self, notification: &Notification) {
        let notification = Arc::new(notification.clone());
        for sender in self.subscribers() {
            match sender.try_send(notification.clone()) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    panic!(
                        "fixture exhausted harness notification capacity; use the real transport for backpressure tests"
                    );
                }
            }
        }
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn receiver_count(&self) -> usize {
        self.subscribers().len()
    }
}

pub(super) struct Publication(Vec<mpsc::OwnedPermit<Arc<Notification>>>);

impl Publication {
    pub(super) fn publish(self, notification: &Arc<Notification>) {
        for permit in self.0 {
            permit.send(notification.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::poll;
    use serde_json::json;

    fn notification(index: usize) -> Arc<Notification> {
        Arc::new(Notification::Other {
            method: format!("test/{index}"),
            params: json!({}),
        })
    }

    #[tokio::test]
    async fn bounded_handoff_waits_and_preserves_order_for_every_subscriber() {
        let fanout = HarnessNotifications::new(2);
        let mut first = fanout.subscribe();
        let mut second = fanout.subscribe();
        fanout.reserve("test").await.publish(&notification(0));
        fanout.reserve("test").await.publish(&notification(1));
        let blocked = fanout.reserve("test");
        tokio::pin!(blocked);
        assert!(poll!(blocked.as_mut()).is_pending());
        assert!(
            matches!(first.recv().await.as_deref(), Some(Notification::Other {method,..}) if method == "test/0")
        );
        assert!(
            poll!(blocked.as_mut()).is_pending(),
            "the other subscriber is still full"
        );
        assert!(
            matches!(second.recv().await.as_deref(), Some(Notification::Other {method,..}) if method == "test/0")
        );
        blocked.await.publish(&notification(2));
        for receiver in [&mut first, &mut second] {
            for index in 1..=2 {
                assert!(
                    matches!(receiver.recv().await.as_deref(), Some(Notification::Other {method,..}) if *method == format!("test/{index}"))
                );
            }
        }
    }

    #[tokio::test]
    async fn dropping_a_full_receiver_releases_publication() {
        let fanout = HarnessNotifications::new(1);
        let slow = fanout.subscribe();
        let mut live = fanout.subscribe();
        fanout.reserve("test").await.publish(&notification(0));
        live.recv().await.unwrap();
        let blocked = fanout.reserve("test");
        tokio::pin!(blocked);
        assert!(poll!(blocked.as_mut()).is_pending());
        drop(slow);
        blocked.await.publish(&notification(1));
        assert!(
            matches!(live.recv().await.as_deref(), Some(Notification::Other {method,..}) if method == "test/1")
        );
    }

    #[tokio::test]
    async fn cancelled_reservation_releases_partial_permits() {
        let fanout = HarnessNotifications::new(1);
        let mut first = fanout.subscribe();
        let mut second = fanout.subscribe();
        fanout.reserve("test").await.publish(&notification(0));
        first.recv().await.unwrap();
        {
            let blocked = fanout.reserve("test");
            tokio::pin!(blocked);
            assert!(poll!(blocked.as_mut()).is_pending());
        }
        second.recv().await.unwrap();
        fanout.reserve("test").await.publish(&notification(1));
        for receiver in [&mut first, &mut second] {
            assert!(
                matches!(receiver.recv().await.as_deref(), Some(Notification::Other {method,..}) if method == "test/1")
            );
        }
    }
}
