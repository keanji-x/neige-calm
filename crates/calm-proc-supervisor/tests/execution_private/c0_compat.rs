use super::*;
use serde_json::Value;

fn journal(h: &Harness, r: &PrepareExecution) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            h.journal()
                .join(format!("{}.execution", r.identity.execution_id.as_str())),
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn c0_compat_v4_same_host_spawned_stays_unknown() {
    recover(false, false).await;
}

#[tokio::test]
async fn c0_compat_v4_changed_boot_settles_releases_and_retires() {
    recover(true, false).await;
}

#[tokio::test]
async fn c0_compat_v4_durably_settled_replays_releases_and_retires() {
    recover(false, true).await;
}

async fn recover(changed_boot: bool, settled: bool) {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    // A real shared attachment provides the v4 receipt read by startup independently of
    // Private journals, whose receipt facts live in the execution journal itself.
    let (_, registration) = external(h.root("upgrade-registration", false), &authority());
    register(&mut c, registration).await;
    let registration_id = &c.registrations().last().unwrap().id;
    let saved: Value = serde_json::from_slice(
        &std::fs::read(
            h.journal()
                .join(format!("{}.registration", registration_id.as_str())),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["contract"], 4);
    let (mut r, held) = private_request(&c, h.root("upgrade-private", false));
    r.specification.program = "/bin/sh".into();
    r.specification.args = vec!["-c".into(), "printf ready; read -r release".into()];
    assert!(matches!(
        c.prepare_private(r.clone(), &held).await.unwrap(),
        ExecutionReply::Prepared
    ));
    let receipt = match c
        .request(ExecutionRequest::Launch(consume(&r)))
        .await
        .unwrap()
    {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            state: ExecutionState::Spawned,
        } => receipt,
        other => panic!("{other:?}"),
    };
    wait_for(&mut c, &r, &receipt, |_, output| output.contains("ready")).await;
    let active = journal(&h, &r);
    assert!(active["phase"].get("Spawned").is_some());
    // Capture the unsettled journal, then positively drain the real child before restart.
    // Replaying a snapshot must never leak a real helper into the test host.
    assert!(matches!(
        c.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&r, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: b"done\n".to_vec()
            },
        }))
        .await
        .unwrap(),
        ExecutionReply::StdinProgress { accepted_bytes: 5 }
    ));
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(c);
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    h.child.wait().unwrap();
    let mut old = if settled { journal(&h, &r) } else { active };
    // Frozen pre-C0 JSON contracts, deliberately independent of the new wire constant.
    assert_eq!(old["version"], 4);
    if changed_boot {
        old["host_boot_id"] = "pre-upgrade-host-boot".into();
    }
    std::fs::write(
        h.journal()
            .join(format!("{}.execution", r.identity.execution_id.as_str())),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let PreparedSource::Private(transfer) = &r.source else {
        unreachable!()
    };
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert_ne!(c.executor_boot(), &r.identity.executor_boot);
    let replay = c.prepare_private(r.clone(), &held).await.unwrap();
    if !changed_boot && !settled {
        assert!(
            matches!(replay, ExecutionReply::Receipt { state: ExecutionState::Unknown { .. }, receipt: Some(ref original) } if original == &receipt)
        );
        assert!(journal(&h, &r)["phase"].get("Unknown").is_some());
        assert!(matches!(
            c.request(ExecutionRequest::Retire {
                execution_id: r.identity.execution_id.clone(),
                lifecycle: r.control_tokens.lifecycle.clone()
            })
            .await
            .unwrap(),
            ExecutionReply::Refused { .. }
        ));
        assert!(
            !h.journal()
                .join(format!("{}.tombstone", r.identity.execution_id.as_str()))
                .exists()
        );
        // The directory remains owned by this unsettled execution even across executor boots.
        let (mut other, _) = private_request(
            &c,
            match &transfer.registration {
                ExecutionRegistration::ExternalDir { root, .. } => root.clone(),
                _ => unreachable!(),
            },
        );
        let ExecutionScope::ExternalDir {
            directory_id: old, ..
        } = &r.identity.scope
        else {
            unreachable!()
        };
        let PreparedSource::Private(t) = &mut other.source else {
            unreachable!()
        };
        let ExecutionRegistration::ExternalDir { directory_id, .. } = &mut t.registration else {
            unreachable!()
        };
        *directory_id = old.clone();
        let ExecutionScope::ExternalDir { directory_id, .. } = &mut other.identity.scope else {
            unreachable!()
        };
        *directory_id = old.clone();
        assert!(matches!(
            c.prepare_private(other, &held).await.unwrap(),
            ExecutionReply::Refused { .. }
        ));
        return;
    }
    if changed_boot {
        assert!(
            matches!(replay, ExecutionReply::Receipt { state: ExecutionState::NoChild { ref reason }, .. } if reason == "host_rebooted")
        );
        assert!(journal(&h, &r)["phase"].get("NoChild").is_some());
    } else {
        assert!(
            matches!(replay, ExecutionReply::Receipt { state: ExecutionState::Drained { .. }, receipt: Some(ref original) } if original == &receipt)
        );
        assert_eq!(journal(&h, &r)["phase"], old["phase"]);
    }
    // The native lease lookup must fail after positive durable settlement.
    assert!(
        c.export(ExportRegistration {
            executor_boot: c.executor_boot().clone(),
            registration: transfer.registration_id.clone(),
            nonce: TransferNonce::fresh().unwrap(),
        })
        .await
        .is_err()
    );
    let mut c = h.connect().await;
    let tombstone = match c
        .request(ExecutionRequest::Retire {
            execution_id: r.identity.execution_id.clone(),
            lifecycle: r.control_tokens.lifecycle.clone(),
        })
        .await
        .unwrap()
    {
        ExecutionReply::Retired(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        tombstone.registration.as_ref(),
        Some(&transfer.registration_id)
    );
    assert_eq!(tombstone.outcome, TombstoneOutcome::Retired);
    assert!(
        h.journal()
            .join(format!("{}.tombstone", r.identity.execution_id.as_str()))
            .exists()
    );
    assert!(
        !h.journal()
            .join(format!("{}.execution", r.identity.execution_id.as_str()))
            .exists()
    );
    assert!(
        matches!(c.request(ExecutionRequest::Inspect(r.identity.execution_id.clone())).await.unwrap(), ExecutionReply::Retired(t) if t == tombstone)
    );
}
