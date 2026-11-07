use super::*;

// The payload has its own finite lifetime even if this test process dies. On unwind,
// ask it to finish and observe cleanup before allowing another test to start.
struct ReleasedPayload {
    root: std::path::PathBuf,
    pid: u32,
}
impl Drop for ReleasedPayload {
    fn drop(&mut self) {
        let _ = std::fs::write(self.root.join("finish"), b"");
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let mut pid = 0i32;
            let (count, errno) = unsafe {
                *libc::__error() = 0;
                let count = libc::proc_listpids(
                    2, /* PROC_PGRP_ONLY, sys/proc_info.h */
                    self.pid,
                    (&mut pid as *mut i32).cast(),
                    std::mem::size_of::<i32>() as i32,
                );
                (count, *libc::__error())
            };
            if count == 0 && errno == 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!("test payload group did not finish within cleanup budget");
                std::process::abort();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
async fn inspect(
    client: &mut RegisteredExecutorConnection,
    r: &PrepareExecution,
) -> ExecutionReply {
    client
        .request(ExecutionRequest::Inspect(r.identity.execution_id.clone()))
        .await
        .unwrap()
}

async fn interrupted_pipe(survivor_only: bool) {
    bounded(async {
        let mut h = Harness::start().await;
        let mut c = h.connect().await;
        let mut r = request(&h, &c, "restart", include_str!("released.sh"));
        r.specification.environment.push(("SURVIVOR_ONLY".into(), u8::from(survivor_only).to_string()));
        let receipt = launch(&mut c, &r).await;
        let PreparedSource::Private(source) = &r.source else { unreachable!() };
        let cleanup = ReleasedPayload { root: source.shape.root.clone(), pid: receipt.process.pid() };
        assert!(matches!(c.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&r, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin { bytes: b"go\n".to_vec() },
        })).await.unwrap(), ExecutionReply::StdinProgress { accepted_bytes: 3 }));
        wait_for(&mut c, &r, &receipt, |_, out| out == "ready").await;
        drop(c);
        h.child.kill().unwrap();
        h.child.wait().unwrap();
        h.child = Harness::spawn(h.temp.path(), &h.socket);
        let mut c = h.connect().await;
        if survivor_only {
            std::fs::write(cleanup.root.join("leader-finish"), b"").unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
                    let (count, errno) = unsafe {
                        *libc::__error() = 0;
                        let count = libc::proc_pidinfo(cleanup.pid as i32, libc::PROC_PIDTBSDINFO, 1,
                            (&mut info as *mut libc::proc_bsdinfo).cast(), std::mem::size_of_val(&info) as i32);
                        (count, *libc::__error())
                    };
                    if count == 0 && errno == libc::ESRCH { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
        }
        // Exceed the normal Stop grace/drain budget: time grants no evidence.
        for _ in 0..26 {
            assert!(matches!(inspect(&mut c, &r).await, ExecutionReply::Inspected {
                state: ExecutionState::Unknown { .. }, output_end: OutputEnd::Incomplete { .. }, ..
            }));
            assert!(matches!(c.request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&r, &receipt, ExecutionControlPurpose::Lifecycle),
                action: ProcessAction::Stop,
            })).await.unwrap(), ExecutionReply::Refused { .. }));
            assert!(matches!(c.request(ExecutionRequest::Retire {
                execution_id: r.identity.execution_id.clone(), lifecycle: r.control_tokens.lifecycle.clone(),
            }).await.unwrap(), ExecutionReply::Refused { .. }));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(matches!(c.request(ExecutionRequest::Launch(consume(&r))).await.unwrap(),
            ExecutionReply::Receipt { state: ExecutionState::Unknown { .. }, .. }));
        let mut replacement = request(&h, &c, "replacement", "printf replacement");
        replacement.identity.process_alias = r.identity.process_alias.clone();
        replacement.handoff = Some(AliasHandoff { previous_execution: r.identity.execution_id.clone(), expected_slot_version: receipt.slot_version });
        assert!(matches!(submit(&mut c, replacement.clone()).await, ExecutionReply::Prepared));
        assert!(matches!(c.request(ExecutionRequest::Launch(consume(&replacement))).await.unwrap(), ExecutionReply::Refused { .. }));
        std::fs::write(cleanup.root.join("finish"), b"").unwrap();
        let settled = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let reply = inspect(&mut c, &r).await;
                if matches!(&reply, ExecutionReply::Inspected { state, .. } if state.permits_handoff()) { break reply; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("dead native scope must recover on the same boot");
        assert!(matches!(settled, ExecutionReply::Inspected {
            state: ExecutionState::InterruptedQuiescent { reason: ExecutionInterruption::ExecutorInterrupted, .. },
            output_end: OutputEnd::Incomplete { .. }, ..
        }));
        if let ExecutionReply::Inspected { state, output_end, .. } = &settled {
            use workspace_runtime::execution::{next_step, Desired, Fact, Step};
            assert!(matches!(state, ExecutionState::InterruptedQuiescent { proof, .. }
                if proof.scope == InterruptedScope::MacOriginalGroupEmpty));
            let fact = Fact::Journal { state: state.clone(), receipt: Some(Box::new(receipt.clone())), output: output_end.clone() };
            for desired in [Desired::Run, Desired::Stop, Desired::Retire] {
                assert_eq!(next_step(desired, &fact), Step::Settle);
            }
            assert!(!Settlement::Journal(state.clone()).descendants_complete());
        }
        assert_eq!(std::fs::read_to_string(cleanup.root.join("launches")).unwrap(), "launched");
        drop(cleanup);
        drop(c);
        h.child.close();
        h.child = Harness::spawn(h.temp.path(), &h.socket);
        let mut c = h.connect().await;
        assert_eq!(serde_json::to_value(inspect(&mut c, &r).await).unwrap(), serde_json::to_value(settled).unwrap());
        let mut replacement = request(&h, &c, "replacement-after-retire", "printf replacement");
        replacement.identity.process_alias = r.identity.process_alias.clone();
        replacement.handoff = Some(AliasHandoff { previous_execution: r.identity.execution_id.clone(), expected_slot_version: receipt.slot_version });
        let next = launch(&mut c, &replacement).await;
        assert!(next.slot_version > receipt.slot_version);
        for _ in 0..2 {
            assert!(matches!(c.request(ExecutionRequest::Retire {
                execution_id: r.identity.execution_id.clone(), lifecycle: r.control_tokens.lifecycle.clone(),
            }).await.unwrap(), ExecutionReply::Retired { .. }));
        }
        assert!(matches!(c.request(ExecutionRequest::Launch(consume(&r))).await.unwrap(), ExecutionReply::Retired { .. }));

        assert!(matches!(c.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&r, &receipt, ExecutionControlPurpose::Lifecycle), action: ProcessAction::Stop,
        })).await.unwrap(), ExecutionReply::Refused { .. }));
        wait_for(&mut c, &replacement, &next, |s, _| s.permits_handoff()).await;
    }).await;
}

#[tokio::test]
async fn recovery_3450_released_pipe_restart_recovers_only_after_native_exit() {
    interrupted_pipe(false).await;
}
#[tokio::test]
async fn recovery_3450_surviving_group_after_leader_exit_stays_unknown() {
    interrupted_pipe(true).await;
}
