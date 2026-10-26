use super::{test_support::*, *};
use std::{future::Future, sync::Arc, time::Duration};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("driver stalled")
}
async fn idle(engine: &Engine<MemoryStore, ScriptedExecutor>, id: &ExecutionId) {
    bounded(async {
        while engine.driving(id) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(engine.error(id), None);
}
fn drained() -> Fact {
    journal(
        ExecutionState::Drained { status: Some(0) },
        OutputEnd::Complete { final_cursor: 0 },
    )
}
fn setup(
    desired: Desired,
    script: Vec<Result<Fact, ExecError>>,
) -> (
    ExecutionIntent,
    MemoryStore,
    ScriptedExecutor,
    Engine<MemoryStore, ScriptedExecutor>,
) {
    let intent = intent("/unused".into());
    let log = CallLog::default();
    let store = MemoryStore::new(log.clone());
    store.seed(intent.clone(), desired);
    let executor = ScriptedExecutor::new(log, script);
    let engine = Engine::new(store.clone(), executor.clone());
    (intent, store, executor, engine)
}
async fn request(
    engine: &Engine<MemoryStore, ScriptedExecutor>,
    store: &MemoryStore,
    id: &ExecutionId,
    desired: Desired,
) {
    let mut tx = MemoryTx::default();
    let pending = engine.request(&mut tx, id, desired).await.unwrap();
    store.commit(tx);
    engine.drive(pending.committed());
}
#[test]
fn next_step_full_table() {
    use Step::*;
    let intent = intent("/unused".into());
    let facts = [
        Fact::Absent,
        journal(ExecutionState::Prepared, OutputEnd::Open),
        journal(ExecutionState::Forking, OutputEnd::Open),
        journal(ExecutionState::Spawned, OutputEnd::Open),
        journal(ExecutionState::Exited { status: None }, OutputEnd::Open),
        journal(ExecutionState::Revoked, OutputEnd::Open),
        journal(
            ExecutionState::NoChild {
                reason: "test".into(),
            },
            OutputEnd::Open,
        ),
        drained(),
        journal(
            ExecutionState::Unknown {
                reason: "test".into(),
            },
            OutputEnd::Open,
        ),
        tombstone(&intent),
    ];
    for (desired, steps) in [
        (
            Desired::Run,
            [
                Prepare, Launch, Wait, Wait, Wait, Settle, Settle, Settle, Wait, Finish,
            ],
        ),
        (
            Desired::Stop,
            [
                Revoke, Revoke, Stop, Stop, Stop, Settle, Settle, Settle, Wait, Finish,
            ],
        ),
        (
            Desired::Retire,
            [
                Revoke, Revoke, Stop, Stop, Stop, Settle, Settle, Settle, Wait, Finish,
            ],
        ),
    ] {
        for (fact, expected) in facts.iter().zip(steps) {
            for output in [
                OutputEnd::Open,
                OutputEnd::Complete { final_cursor: 0 },
                OutputEnd::Incomplete {
                    reason: "restart".into(),
                },
            ] {
                let mut fact = fact.clone();
                if let Fact::Journal {
                    output: current, ..
                } = &mut fact
                {
                    *current = output;
                }
                assert_eq!(next_step(desired, &fact), expected, "{desired:?} {fact:?}");
            }
        }
    }
}

#[test]
fn native_macos_settlement_requires_complete_output_and_keeps_unknown() {
    for desired in [Desired::Run, Desired::Stop, Desired::Retire] {
        for output in [
            OutputEnd::Open,
            OutputEnd::Incomplete {
                reason: "reader failed".into(),
            },
            OutputEnd::Complete { final_cursor: 7 },
        ] {
            let fact = journal(
                ExecutionState::OriginalGroupEmpty { status: Some(0) },
                output.clone(),
            );
            assert_eq!(
                next_step(desired, &fact),
                if matches!(output, OutputEnd::Complete { .. }) {
                    Step::Settle
                } else {
                    Step::Wait
                }
            );
            assert_eq!(
                next_step(
                    desired,
                    &journal(
                        ExecutionState::Unknown {
                            reason: "lost handle".into()
                        },
                        output
                    )
                ),
                Step::Wait
            );
        }
    }
}
#[tokio::test]
async fn absent_stop_unknown_revoke_finishes() {
    let (intent, store, executor, engine) = setup(Desired::Stop, vec![Ok(Fact::Absent)]);
    executor.push(Ok(tombstone(&intent)));
    executor.push(Ok(tombstone(&intent)));
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    idle(&engine, &intent.metadata.id).await;
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Finished { .. }
    ));
    assert_eq!(
        executor.log.calls(),
        [
            "load", "load", "inspect", "revoke", "load", "load", "inspect", "settle", "finish",
            "load"
        ]
    );
}
#[tokio::test]
async fn drained_settles_before_retire() {
    let (intent, store, executor, engine) = setup(
        Desired::Retire,
        vec![
            Ok(journal(
                ExecutionState::Drained { status: Some(0) },
                OutputEnd::Open,
            )),
            Err(ExecError::Busy(BusyReason::OutputOpen)),
        ],
    );
    let pause = Arc::new(Pause::default());
    *executor.retire_pause.lock().unwrap() = Some(pause.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    bounded(pause.entered.notified()).await;
    assert!(
        store
            .load(&intent.metadata.id)
            .await
            .unwrap()
            .unwrap()
            .is_settled()
    );
    executor.push(Ok(drained()));
    executor.push(Ok(tombstone(&intent)));
    executor.push(Ok(tombstone(&intent)));
    executor.changed(); // transition while Busy is in flight must not be lost
    pause.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Finished { .. }
    ));
    let calls = executor.log.calls();
    assert!(calls.iter().position(|c| c == "settle") < calls.iter().position(|c| c == "retire"));
    assert_eq!(calls.iter().filter(|c| *c == "settle").count(), 1);
    assert_eq!(calls.iter().filter(|c| *c == "retire").count(), 2);
    assert_eq!(calls.iter().filter(|c| *c == "finish").count(), 1);
}
#[tokio::test]
async fn drained_between_inspect_and_wait_wakes() {
    let (intent, store, executor, engine) = setup(
        Desired::Run,
        vec![
            Ok(journal(
                ExecutionState::Unknown {
                    reason: "wait".into(),
                },
                OutputEnd::Open,
            )),
            Ok(drained()),
        ],
    );
    let pause = Arc::new(Pause::default());
    *executor.inspect_pause.lock().unwrap() = Some(pause.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    bounded(pause.entered.notified()).await;
    executor.changed();
    pause.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert!(
        store
            .load(&intent.metadata.id)
            .await
            .unwrap()
            .unwrap()
            .is_settled()
    );
    assert_eq!(
        executor
            .log
            .calls()
            .iter()
            .filter(|c| *c == "inspect")
            .count(),
        2
    );
    assert!(executor.log.calls().contains(&"settle".into()));
    assert!(!executor.log.calls().contains(&"finish".into()));
}
#[tokio::test]
async fn request_after_park_decision_is_not_lost() {
    let (intent, store, executor, engine) = setup(Desired::Run, vec![Ok(drained()), Ok(drained())]);
    executor.push(Ok(tombstone(&intent)));
    executor.push(Ok(tombstone(&intent)));
    let pause = Arc::new(Pause::default());
    *store.settle_pause.lock().unwrap() = Some(pause.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    bounded(pause.entered.notified()).await;
    request(&engine, &store, &intent.metadata.id, Desired::Retire).await;
    pause.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Finished { .. }
    ));
    assert_eq!(
        executor
            .log
            .calls()
            .iter()
            .filter(|c| *c == "retire")
            .count(),
        1
    );
}
#[tokio::test]
async fn retire_between_load_and_park_drives() {
    let (intent, store, executor, engine) = setup(Desired::Run, vec![Ok(drained()), Ok(drained())]);
    store
        .settle(
            &intent.metadata.id,
            &Settlement::Journal(ExecutionState::Drained { status: Some(0) }),
        )
        .await
        .unwrap();
    executor.push(Ok(tombstone(&intent)));
    executor.push(Ok(tombstone(&intent)));
    let pause = Arc::new(Pause::default());
    *store.load_pause.lock().unwrap() = Some(pause.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    bounded(pause.entered.notified()).await;
    request(&engine, &store, &intent.metadata.id, Desired::Retire).await;
    pause.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Finished { .. }
    ));
    assert_eq!(
        executor
            .log
            .calls()
            .iter()
            .filter(|c| *c == "retire")
            .count(),
        1
    );
}
#[tokio::test]
async fn recover_racing_drive_effect_once() {
    let (intent, store, executor, engine) = setup(Desired::Stop, vec![Ok(Fact::Absent)]);
    executor.push(Ok(tombstone(&intent)));
    executor.push(Ok(tombstone(&intent)));
    let pause = Arc::new(Pause::default());
    *executor.inspect_pause.lock().unwrap() = Some(pause.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    bounded(pause.entered.notified()).await;
    assert_eq!(engine.recover().await.scheduled, 1);
    pause.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert_eq!(
        executor
            .log
            .calls()
            .iter()
            .filter(|c| *c == "revoke")
            .count(),
        1
    );
    assert_eq!(
        executor
            .log
            .calls()
            .iter()
            .filter(|c| *c == "finish")
            .count(),
        1
    );
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Finished { .. }
    ));
}
#[tokio::test]
async fn admission_has_no_effect_before_commit_and_desired_never_decreases() {
    let (intent, store, executor, engine) = setup(Desired::Run, vec![]);
    let fresh = test_support::intent("/unused".into());
    let mut tx = MemoryTx::default();
    let ticket = engine.admit(&mut tx, fresh.clone()).await.unwrap();
    assert!(store.load(&fresh.metadata.id).await.unwrap().is_none());
    assert!(!executor.log.calls().contains(&"prepare".into()));
    drop(ticket); // a rollback discards the pending ticket
    let mut tx = MemoryTx::default();
    let _ = engine
        .request(&mut tx, &intent.metadata.id, Desired::Retire)
        .await
        .unwrap();
    let _ = engine
        .request(&mut tx, &intent.metadata.id, Desired::Run)
        .await
        .unwrap();
    store.commit(tx);
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap().unwrap(),
        Stored::Active {
            desired: Desired::Retire,
            ..
        }
    ));
}
mod port_contract {
    use super::*;
    #[tokio::test]
    async fn executor() {
        let intent = intent("/unused".into());
        let retired = tombstone(&intent);
        let fake = ScriptedExecutor::new(
            CallLog::default(),
            [
                Ok(Fact::Absent),
                Ok(retired.clone()),
                Ok(retired.clone()),
                Ok(retired.clone()),
                Err(ExecError::Refused("retired".into())),
                Ok(retired),
            ],
        );
        let launch = test_support::intent("/unused".into());
        let receipt = ExecutionReceipt {
            identity: ExecutionIdentity {
                execution_id: launch.metadata.id.clone(),
                executor_boot: ExecutorBootId::fresh().unwrap(),
                ticket_id: launch.metadata.ticket_id.clone(),
                source_registration: launch.metadata.source.registration.clone(),
                source_incarnation: SourceObjectIncarnation::ExternalDir(
                    launch.metadata.source.incarnation.clone(),
                ),
                authority: launch.metadata.source.authority.clone(),
                scope: ExecutionScope::ExternalDir {
                    directory_id: launch.metadata.source.directory.clone(),
                    registration_version: 1,
                    cwd: launch.metadata.source.cwd.clone(),
                },
                operation_id: launch.metadata.owner.0.clone(),
                specification_id: launch.metadata.specification.clone(),
                process_alias: launch.metadata.alias.0.clone(),
            },
            slot_version: 1,
            process: NativeProcessIdentity::LinuxHelper(
                workspace::execution::LinuxProcessIdentity {
                    pid: 1,
                    start_time_ticks: 1,
                    nonce: CapabilityToken::fresh().unwrap(),
                },
            ),
            scope_mechanism: ScopeMechanismIdentity::LinuxHelper(
                workspace::execution::LinuxHelperIdentity {
                    executable_device: 1,
                    executable_inode: 1,
                    protocol_version: 1,
                },
            ),
        };
        let mut retired = match tombstone(&launch) {
            Fact::Retired(t) => t,
            _ => unreachable!(),
        };
        retired.outcome = TombstoneOutcome::Retired;
        for fact in [
            Ok(Fact::Absent),
            Ok(journal(ExecutionState::Prepared, OutputEnd::Open)),
            Ok(journal(ExecutionState::Prepared, OutputEnd::Open)),
            Err(ExecError::Busy(BusyReason::OutputOpen)),
            Ok(Fact::Journal {
                state: ExecutionState::Spawned,
                receipt: Some(Box::new(receipt)),
                output: OutputEnd::Open,
            }),
            Ok(drained()),
            Ok(drained()),
            Ok(Fact::Retired(retired.clone())),
            Ok(Fact::Retired(retired.clone())),
            Ok(Fact::Retired(retired)),
            Err(ExecError::Refused("retired".into())),
        ] {
            fake.push(fact);
        }
        test_support::port_contract::executor(&fake, &intent, &launch).await;
        assert_eq!(
            fake.log.calls(),
            [
                "inspect", "revoke", "inspect", "revoke", "prepare", "retire", "inspect",
                "prepare", "prepare", "retire", "launch", "control", "inspect", "retire",
                "inspect", "retire", "prepare"
            ]
        );
    }
}
#[cfg(feature = "session-executor")]
mod watch_tests;

#[tokio::test]
async fn finished_record_has_no_secrets() {
    let (intent, store, _, _) = setup(Desired::Retire, vec![]);
    store
        .settle(
            &intent.metadata.id,
            &Settlement::from_fact(&tombstone(&intent)),
        )
        .await
        .unwrap();
    store.finish(&intent.metadata.id).await.unwrap();
    let row = store.load(&intent.metadata.id).await.unwrap().unwrap();
    let Stored::Finished {
        intent: metadata,
        settlement,
    } = row
    else {
        panic!("finish must discard the active record and its secrets");
    };
    assert_eq!(metadata.id, intent.metadata.id);
    assert_eq!(metadata.ticket_id, intent.metadata.ticket_id);
    assert_eq!(settlement, Settlement::from_fact(&tombstone(&intent)));
    assert!(store.pending().await.unwrap().is_empty());
    store.finish(&intent.metadata.id).await.unwrap(); // idempotent
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap(),
        Some(Stored::Finished { .. })
    ));
}

#[tokio::test]
async fn drive_after_finish_is_noop() {
    let (intent, store, executor, engine) = setup(Desired::Retire, vec![]);
    store
        .settle(
            &intent.metadata.id,
            &Settlement::from_fact(&tombstone(&intent)),
        )
        .await
        .unwrap();
    store.finish(&intent.metadata.id).await.unwrap();
    let pause = Arc::new(Pause::default());
    *executor.generation_pause.lock().unwrap() = Some(pause);
    for _ in 0..2 {
        engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
        idle(&engine, &intent.metadata.id).await;
    }
    assert_eq!(executor.log.calls(), ["settle", "finish", "load", "load"]);
}

#[tokio::test]
async fn wait_settled_after_finish_returns_settled() {
    let (intent, store, _, engine) = setup(Desired::Retire, vec![]);
    store
        .settle(
            &intent.metadata.id,
            &Settlement::from_fact(&tombstone(&intent)),
        )
        .await
        .unwrap();
    store.finish(&intent.metadata.id).await.unwrap();
    assert_eq!(engine.wait_settled(&intent.metadata.id).await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn wait_settled_times_out_unsettled() {
    let (intent, store, executor, engine) = setup(Desired::Run, vec![]);
    let start = tokio::time::Instant::now();
    assert_eq!(
        engine.wait_settled(&intent.metadata.id).await,
        Err(DriveError::Timeout)
    );
    assert_eq!(start.elapsed(), QUIESCE_DEADLINE);
    assert!(matches!(
        store.load(&intent.metadata.id).await.unwrap(),
        Some(Stored::Active {
            settlement: None,
            ..
        })
    ));
    assert_eq!(executor.log.calls(), ["load", "load"]);
}

#[tokio::test(start_paused = true)]
async fn wait_settled_cancel_keeps_driver() {
    let (intent, store, executor, engine) = setup(
        Desired::Run,
        vec![Ok(journal(
            ExecutionState::Unknown {
                reason: "waiting".into(),
            },
            OutputEnd::Open,
        ))],
    );
    let inspect = Arc::new(Pause::default());
    *executor.inspect_pause.lock().unwrap() = Some(inspect.clone());
    engine.drive(DriveTicket::committed(intent.metadata.id.clone()));
    inspect.entered.notified().await;
    let load = Arc::new(Pause::default());
    *store.load_pause.lock().unwrap() = Some(load.clone());
    let waiter = {
        let engine = engine.clone();
        let id = intent.metadata.id.clone();
        tokio::spawn(async move { engine.wait_settled(&id).await })
    };
    load.entered.notified().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(engine.driving(&intent.metadata.id));
    executor.push(Ok(drained()));
    executor.changed();
    inspect.resume.notify_one();
    idle(&engine, &intent.metadata.id).await;
    assert_eq!(engine.wait_settled(&intent.metadata.id).await, Ok(()));
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

mod recovery_3192;
