//! Failure budgets for the plugin-owned one-shot handshake, not scheduling delays.
pub(super) async fn reached(receiver: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(10), receiver)
        .await
        .expect("supervisor handshake timed out")
        .expect("supervisor dropped handshake before reaching it");
}
