use super::*;

fn enabled() -> PairingState {
    PairingState {
        origin: Some("https://pair.example.ts.net".into()),
        ..PairingState::default()
    }
}

fn claimed(state: &mut PairingState) -> PairingClaimed {
    let (_, payload, _) = state.invite().unwrap();
    let ticket = payload.split("#v1.").nth(1).unwrap().into();
    state
        .claim(PairingClaim {
            ticket,
            device_name: "Test phone".into(),
        })
        .unwrap()
}

#[test]
fn mobile_pairing_requires_approval_and_one_time_redemption() {
    let sessions = SessionStore::new();
    let mut state = enabled();
    let claim = claimed(&mut state);
    let request = || PairingRedeem {
        id: claim.id.clone(),
        secret: claim.secret.clone(),
    };
    assert!(state.redeem(request(), &sessions).unwrap().is_none());
    assert!(
        state
            .redeem(
                PairingRedeem {
                    id: claim.id.clone(),
                    secret: "0".repeat(64)
                },
                &sessions
            )
            .is_err()
    );
    state.approve(&claim.id).unwrap();
    let session = state.redeem(request(), &sessions).unwrap().unwrap();
    assert!(sessions.get(&session).is_some());
    assert!(state.redeem(request(), &sessions).is_err());
}

#[test]
fn mobile_pairing_claim_cannot_be_replaced() {
    let mut state = enabled();
    let (_, payload, _) = state.invite().unwrap();
    let ticket = payload.split("#v1.").nth(1).unwrap().to_owned();
    let first = state
        .claim(PairingClaim {
            ticket: ticket.clone(),
            device_name: "First".into(),
        })
        .unwrap();
    assert!(
        state
            .claim(PairingClaim {
                ticket,
                device_name: "Replacement".into()
            })
            .is_err()
    );
    let pending = state.list().0;
    assert_eq!(pending[0].device_name, "First");
    assert_eq!(pending[0].verification_code, first.verification_code);
}

#[test]
fn mobile_pairing_disable_revokes_sessions_claims_and_live_transports() {
    let sessions = SessionStore::new();
    let mut state = enabled();
    let claim = claimed(&mut state);
    state.approve(&claim.id).unwrap();
    let session = state
        .redeem(
            PairingRedeem {
                id: claim.id,
                secret: claim.secret,
            },
            &sessions,
        )
        .unwrap()
        .unwrap();
    let pending = claimed(&mut state);
    state.approve(&pending.id).unwrap();
    let connection = state.connections.clone();
    state.disable(&sessions);
    assert!(connection.is_cancelled());
    assert!(sessions.get(&session).is_none());
    state.origin = Some("https://pair.example.ts.net".into());
    assert!(
        state
            .redeem(
                PairingRedeem {
                    id: pending.id,
                    secret: pending.secret
                },
                &sessions
            )
            .is_err()
    );
    assert!(state.list().1.is_empty());
}

#[test]
fn mobile_pairing_expiry_and_capacity_are_enforced() {
    let mut state = enabled();
    let (_, payload, _) = state.invite().unwrap();
    for row in state.pending.values_mut() {
        row.expires = Instant::now() - Duration::from_secs(1);
    }
    assert!(
        state
            .claim(PairingClaim {
                ticket: payload.split("#v1.").nth(1).unwrap().into(),
                device_name: "Expired".into()
            })
            .is_err()
    );
    for _ in 0..MAX_PENDING {
        state.invite().unwrap();
    }
    assert!(state.invite().is_err());
}

#[test]
fn mobile_pairing_device_revocation_removes_only_its_session_and_closes_streams() {
    let sessions = SessionStore::new();
    let mut state = enabled();
    let owner_session = sessions.create();
    let claim = claimed(&mut state);
    state.approve(&claim.id).unwrap();
    let mobile_session = state
        .redeem(
            PairingRedeem {
                id: claim.id,
                secret: claim.secret,
            },
            &sessions,
        )
        .unwrap()
        .unwrap();
    let device = state.list().1.remove(0);
    let transport = state.connections.clone();
    state.revoke(&device.id, &sessions).unwrap();
    assert!(sessions.get(&mobile_session).is_none());
    assert!(sessions.get(&owner_session).is_some());
    assert!(transport.is_cancelled());
}
