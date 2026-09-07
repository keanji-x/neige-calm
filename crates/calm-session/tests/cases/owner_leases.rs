use super::*;

#[test]
fn takeover_revokes_the_previous_owners_mutating_frames() {
    let broadcaster = PtyBroadcaster::new(1024);
    let context = ctx(&broadcaster, Uuid::new_v4());
    let mut registry = OwnerRegistry::new();
    let mut previous = TerminalSessionState::new();
    let mut next = TerminalSessionState::new();
    previous.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    next.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    next.on_client_frame(
        ClientMsg::OwnerClaim,
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    for frame in [
        ClientMsg::Input {
            data: b"stale".to_vec(),
            input_seq: 1,
        },
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 30,
        },
        ClientMsg::Kill,
        ClientMsg::TerminalThemeUpdate {
            fg: (1, 2, 3),
            bg: (4, 5, 6),
        },
    ] {
        let effects =
            previous.on_client_frame(frame.clone(), broadcaster.buffer(), &mut registry, &context);
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::SendToClient(DaemonMsg::ProtocolError {
                    code: ProtocolErrorCode::NotOwner,
                    ..
                }) | Effect::SendProtocolError {
                    code: ProtocolErrorCode::NotOwner,
                    ..
                }
            )),
            "stale {frame:?} was not refused: {effects:?}"
        );
        assert!(
            !effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteToPty { .. }
                    | Effect::ResizePty { .. }
                    | Effect::KillChild
                    | Effect::TerminalThemeUpdate { .. }
            )),
            "stale {frame:?} emitted an IO effect: {effects:?}"
        );
    }
}

#[test]
fn reconnect_with_same_client_id_gets_a_fresh_owner_claim() {
    let broadcaster = PtyBroadcaster::new(1024);
    let context = ctx(&broadcaster, Uuid::new_v4());
    let mut registry = OwnerRegistry::new();
    let client_id = Uuid::new_v4();
    let mut old = TerminalSessionState::new();
    let mut new = TerminalSessionState::new();
    old.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    new.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    let effects = new.on_client_frame(
        ClientMsg::OwnerClaim,
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    assert!(
        effects.contains(&Effect::BroadcastOwnerChanged(Some(client_id))),
        "reconnected observer needs its promotion acknowledgement even when the client ID is unchanged"
    );
    old.on_client_frame(
        ClientMsg::OwnerRelease,
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    assert_eq!(
        registry.current_owner(),
        Some(client_id),
        "old connection released the new owner"
    );
}

#[test]
fn stale_connection_disconnect_cannot_release_a_reconnected_owner() {
    let broadcaster = PtyBroadcaster::new(1024);
    let context = ctx(&broadcaster, Uuid::new_v4());
    let mut registry = OwnerRegistry::new();
    let client_id = Uuid::new_v4();
    let mut old = TerminalSessionState::new();
    let mut new = TerminalSessionState::new();
    old.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    new.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    new.on_client_frame(
        ClientMsg::OwnerClaim,
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    assert!(!old.release_owner(&mut registry));
    assert_eq!(registry.current_owner(), Some(client_id));
    let input = ClientMsg::Input {
        data: b"current".to_vec(),
        input_seq: 1,
    };
    assert_eq!(
        new.on_client_frame(input, broadcaster.buffer(), &mut registry, &context),
        vec![Effect::WriteToPty {
            data: b"current".to_vec(),
            input_seq: 1
        }]
    );
    assert!(new.release_owner(&mut registry));
    assert_eq!(registry.current_owner(), None);
}

#[test]
fn same_id_takeover_revokes_old_input_even_before_old_release() {
    let broadcaster = PtyBroadcaster::new(1024);
    let context = ctx(&broadcaster, Uuid::new_v4());
    let mut registry = OwnerRegistry::new();
    let client_id = Uuid::new_v4();
    let mut old = TerminalSessionState::new();
    let mut new = TerminalSessionState::new();
    old.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    new.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    new.on_client_frame(
        ClientMsg::OwnerClaim,
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    let effects = old.on_client_frame(
        ClientMsg::Input {
            data: b"stale".to_vec(),
            input_seq: 1,
        },
        broadcaster.buffer(),
        &mut registry,
        &context,
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::SendProtocolError {
            code: ProtocolErrorCode::NotOwner,
            ..
        }]
    ));
}
