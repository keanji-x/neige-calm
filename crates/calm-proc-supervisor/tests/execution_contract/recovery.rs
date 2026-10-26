use super::*;
use std::time::Duration;

async fn inspect(c: &mut RegisteredExecutorConnection, r: &PrepareExecution) -> ExecutionReply {
    c.request(ExecutionRequest::Inspect(r.identity.execution_id.clone()))
        .await
        .unwrap()
}

async fn observe_unknown(
    c: &mut RegisteredExecutorConnection,
    r: &PrepareExecution,
) -> ExecutionReply {
    // Retry uncertainty from unrelated disappearing /proc entries as production
    // recovery does. A single uncertain scan must not mask an unsafe acceptance.
    for _ in 0..30 {
        let reply = inspect(c, r).await;
        if !matches!(
            reply,
            ExecutionReply::Inspected {
                state: ExecutionState::Unknown { .. },
                ..
            }
        ) {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    inspect(c, r).await
}

#[tokio::test]
async fn recovery_3192_dead_helper_restart_retires_idempotently() {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("interrupted", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "interrupted",
        IoMode::Pipe,
        "printf launched >> launches; printf ready; read release",
    );
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |_, out| out.contains("ready")).await;
    // Kill the actual owned session, then let its owning executor reap its helper.
    assert_eq!(
        unsafe { libc::kill(-(receipt.process.pid() as i32), libc::SIGKILL) },
        0
    );
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Unknown { .. })
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::path::Path::new(&format!("/proc/{}", receipt.process.pid())).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(c);
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let fact = inspect(&mut c, &r).await;
            if matches!(&fact, ExecutionReply::Inspected { state, .. } if state.permits_handoff()) {
                break fact;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dead scope must settle after restart");
    match &recovered {
        ExecutionReply::Inspected {
            receipt: original,
            state,
            output_end,
        } => {
            assert_eq!(original.as_ref(), Some(&receipt));
            assert!(
                state.permits_handoff(),
                "dead scope must settle after restart: {state:?}"
            );
            assert!(
                serde_json::to_value(state)
                    .unwrap()
                    .get("InterruptedQuiescent")
                    .is_some()
            );
            assert!(matches!(output_end, OutputEnd::Incomplete { .. }));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(inspect(&mut c, &r).await).unwrap(),
        serde_json::to_value(recovered).unwrap()
    );
    drop(c);
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert!(
        matches!(inspect(&mut c, &r).await, ExecutionReply::Inspected { state:ExecutionState::InterruptedQuiescent { .. }, receipt:Some(ref original), .. } if original == &receipt)
    );
    let ExecutionRegistration::ExternalDir { root, .. } = registration else {
        unreachable!()
    };
    assert_eq!(
        std::fs::read_to_string(root.path.join("launches")).unwrap(),
        "launched"
    );
    for _ in 0..2 {
        assert!(matches!(
            c.request(ExecutionRequest::Retire {
                execution_id: r.identity.execution_id.clone(),
                lifecycle: r.control_tokens.lifecycle.clone(),
            })
            .await
            .unwrap(),
            ExecutionReply::Retired(_)
        ));
    }
}

struct ReleaseOnDrop(std::path::PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

async fn surviving_member(session_only: bool) {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let root = h.root("survivor", false);
    let _release = ReleaseOnDrop(root.path.join("release"));
    let (scope, registration) = external(root, &grant);
    let mut r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "survivor",
        IoMode::Pipe,
        "",
    );
    r.specification.program = "/usr/bin/python3".into();
    r.specification.args = vec![
        "-c".into(),
        format!(
            "import os,time\nsid=os.getsid(0)\n{}\nprint('ready',flush=True)\nos.kill(sid,9)\nwhile not os.path.exists('release'): time.sleep(.01)",
            if session_only {
                "os.setpgid(0,0)"
            } else {
                "pass"
            }
        ),
    ];
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |state, out| {
        matches!(state, ExecutionState::Unknown { .. }) && out.contains("ready")
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::path::Path::new(&format!("/proc/{}", receipt.process.pid())).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(c);
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert!(matches!(
        observe_unknown(&mut c, &r).await,
        ExecutionReply::Inspected {
            state: ExecutionState::Unknown { .. },
            ..
        }
    ));
    assert!(matches!(
        c.request(ExecutionRequest::Retire {
            execution_id: r.identity.execution_id.clone(),
            lifecycle: r.control_tokens.lifecycle.clone()
        })
        .await
        .unwrap(),
        ExecutionReply::Refused { .. }
    ));
}

#[tokio::test]
async fn recovery_3192_surviving_pgid_stays_unknown() {
    surviving_member(false).await;
}
#[tokio::test]
async fn recovery_3192_surviving_session_stays_unknown() {
    surviving_member(true).await;
}

#[tokio::test]
async fn recovery_3192_live_identity_stays_unknown() {
    // A persisted native identity may now belong to a process outside the original group/session.
    // Isolate identity liveness from membership so each independent rejection is observable.
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("identity", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "identity",
        IoMode::Pipe,
        "printf ready; read release",
    );
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |_, out| out.contains("ready")).await;
    c.close_stdin(capability(
        &r,
        &receipt,
        ExecutionControlPurpose::Interactive,
    ))
    .await
    .unwrap();
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(c);
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let path = h
        .journal()
        .join(format!("{}.execution", r.identity.execution_id.as_str()));
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut receipt = receipt;
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
    let ticks = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    let NativeProcessIdentity::LinuxHelper(ref mut native) = receipt.process else {
        unreachable!()
    };
    native.pid = child.id();
    native.start_time_ticks = ticks;
    journal["phase"] = serde_json::json!({"Spawned": receipt});
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    let reply = observe_unknown(&mut c, &r).await;
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        matches!(
            reply,
            ExecutionReply::Inspected {
                state: ExecutionState::Unknown { .. },
                ..
            }
        ),
        "{reply:?}"
    );
}

#[tokio::test]
async fn recovery_3192_missing_receipt_stays_unknown() {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let grant = authority();
    let (scope, registration) = external(h.root("receiptless", false), &grant);
    let r = prepare(
        &c,
        scope,
        &registration,
        grant,
        "receiptless",
        IoMode::Pipe,
        "printf ready; read release",
    );
    let receipt = launch(&mut c, &r).await;
    c.close_stdin(capability(
        &r,
        &receipt,
        ExecutionControlPurpose::Interactive,
    ))
    .await
    .unwrap();
    wait_for(&mut c, &r, &receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(c);
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    let path = h
        .journal()
        .join(format!("{}.execution", r.identity.execution_id.as_str()));
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal["phase"] = serde_json::json!({"Forking":{"slot_version":receipt.slot_version}});
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert!(matches!(
        inspect(&mut c, &r).await,
        ExecutionReply::Inspected {
            receipt: None,
            state: ExecutionState::Unknown { .. },
            ..
        }
    ));
}
