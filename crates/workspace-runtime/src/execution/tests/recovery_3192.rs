use super::*;

fn interrupted() -> Fact {
    let state = serde_json::from_value(serde_json::json!({"InterruptedQuiescent": {
        "reason": "ExecutorInterrupted",
        "proof": {
            "host_boot_id": "test-boot",
            "helper_process": {"pid": 12345, "start_time_ticks": 1, "nonce": CapabilityToken::fresh().unwrap()},
            "mechanism": {"executable_device": 1, "executable_inode": 2, "protocol_version": 1},
            "scope": "LinuxOriginalGroupAndSessionEmpty"
        }
    }})).expect("interrupted settlement must be a typed execution state");
    journal(
        state,
        OutputEnd::Incomplete {
            reason: "executor_restarted".into(),
        },
    )
}

#[tokio::test]
async fn recovery_3192_settles_for_every_desire_and_retires() {
    for desired in [Desired::Run, Desired::Stop, Desired::Retire] {
        let fact = interrupted();
        assert_eq!(next_step(desired, &fact), Step::Settle);
        let (intent, store, executor, engine) = setup(desired, vec![Ok(fact.clone())]);
        if desired == Desired::Retire {
            executor.push(Ok(tombstone(&intent)));
            executor.push(Ok(tombstone(&intent)));
        }
        engine.recover().await;
        idle(&engine, &intent.metadata.id).await;
        assert!(
            store
                .load(&intent.metadata.id)
                .await
                .unwrap()
                .unwrap()
                .is_settled()
        );
        let calls = executor.log.calls();
        assert_eq!(calls.iter().filter(|c| *c == "settle").count(), 1);
        assert!(!calls.iter().any(|c| c == "launch" || c == "prepare"));
        assert_eq!(
            calls.iter().filter(|c| *c == "finish").count(),
            usize::from(desired == Desired::Retire)
        );
    }
}

#[tokio::test]
async fn recovery_3192_existing_active_and_finished_settlement_is_immutable() {
    for finished in [false, true] {
        let (intent, store, executor, engine) = setup(Desired::Retire, vec![Ok(interrupted())]);
        let legacy = Settlement::Journal(ExecutionState::Drained { status: None });
        store.settle(&intent.metadata.id, &legacy).await.unwrap();
        if finished {
            store.finish(&intent.metadata.id).await.unwrap();
        } else {
            executor.push(Ok(tombstone(&intent)));
            executor.push(Ok(tombstone(&intent)));
        }
        engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
        idle(&engine, &intent.metadata.id).await;
        let row = store.load(&intent.metadata.id).await.unwrap().unwrap();
        assert!(matches!(row, Stored::Finished { settlement, .. } if settlement == legacy));
        assert_eq!(
            executor
                .log
                .calls()
                .iter()
                .filter(|c| *c == "settle")
                .count(),
            1
        );
    }
}
