use super::*;

async fn settled(daemon: &SharedCodexAppServer, peer: &mut Peer, id: &str) {
    notify(daemon, peer, "turn/started", json!({"id":id})).await;
    let terminal = json!({"id":id,"status":"completed"});
    notify(daemon, peer, "turn/completed", terminal).await;
}

#[tokio::test]
async fn carrier_quiescence_codex_source_publication_invalidates_lease() {
    let daemon = daemon().await;
    let (_old, mut old_peer, forwarder) = connect_with_reader(&daemon).await;
    settled(&daemon, &mut old_peer, "old").await;
    let guard = daemon.carrier_idle_for_detach("thread").unwrap();
    let (_new, mut peer) = connect(&daemon).await;
    // A cancelled old forwarder must not discard the current new source.
    forwarder.abort();
    assert!(forwarder.await.unwrap_err().is_cancelled());
    assert!(!guard.recheck());
    drop(guard);
    assert!(daemon.carrier_idle_for_detach("thread").is_none());
    notify(&daemon, &mut peer, "turn/started", json!({"id":"old"})).await;
    assert!(daemon.carrier_idle_for_detach("thread").is_none());
    let terminal = json!({"id":"old","status":"completed"});
    notify(&daemon, &mut peer, "turn/completed", terminal).await;
    let guard = daemon.carrier_idle_for_detach("thread").unwrap();
    daemon.native_turns.remove("thread");
    assert!(guard.recheck());
    drop(guard);
    assert!(daemon.carrier_idle_for_detach("thread").is_none());
}

#[tokio::test]
async fn carrier_quiescence_codex_eof_before_retirement_fails_recheck() {
    let daemon = daemon().await;
    let (client, mut peer, forwarder) = connect_with_reader(&daemon).await;
    settled(&daemon, &mut peer, "settled").await;
    let guard = daemon.carrier_idle_for_detach("thread").unwrap();
    let start = daemon
        .native_turns
        .prepare_start(&client, "thread", NativeInput::Legacy);
    assert!(start.is_err());
    daemon.native_turns.invalidate();
    daemon.native_turns.publish(&client);
    assert!(!guard.recheck());
    drop(guard);
    let guard = daemon.carrier_idle_for_detach("thread").unwrap();
    // Block the forwarder on attribution until the reader reaches EOF.
    let attribution = hold_attribution(&daemon).await;
    send(
        &mut peer,
        json!({"method":"thread/started", "params":{"thread":{"id":"delayed"}}}),
    )
    .await;
    attribution_blocked(&daemon).await;
    drop(peer);
    reader_finished(&client).await;
    assert!(
        !guard.recheck(),
        "EOF is not a current source even before retirement"
    );
    drop(attribution);
    forwarder.await.unwrap();
}

#[tokio::test]
async fn carrier_quiescence_codex_retry_history_preserves_detach_source() {
    let daemon = daemon().await;
    let (client, mut peer) = connect(&daemon).await;
    let owner = &daemon.native_turns;
    let predecessor = owner
        .prepare_start(&client, "thread", NativeInput::Legacy)
        .unwrap();
    owner.accepted(&client, "thread", &predecessor, "predecessor");
    let successor = owner
        .prepare_start(&client, "thread", NativeInput::Legacy)
        .unwrap();
    // END before ACK is legal. The history owner must retain its source when
    // accepting the successor, and a late predecessor END must stay separate.
    settled(&daemon, &mut peer, "successor").await;
    owner.accepted(&client, "thread", &successor, "successor");
    let guard = daemon.carrier_idle_for_detach("thread").unwrap();
    notify(
        &daemon,
        &mut peer,
        "turn/completed",
        json!({"id":"predecessor","status":"completed"}),
    )
    .await;
    assert!(guard.recheck());
    assert!(
        owner
            .prepare_start(&client, "thread", NativeInput::Legacy)
            .is_err()
    );
    drop(guard);
    assert!(daemon.carrier_idle_for_detach("thread").is_some());
}
