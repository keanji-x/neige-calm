use super::execution_support::*;
use calm_session::control::{IoMode, execution::*};
use std::time::Duration;

fn journal(h: &Harness, r: &PrepareExecution) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(
            h.journal()
                .join(format!("{}.execution", r.identity.execution_id.as_str())),
        )
        .unwrap(),
    )
    .unwrap()
}

async fn setup(
    h: &Harness,
    mode: IoMode,
    script: &str,
) -> (
    RegisteredExecutorConnection,
    PrepareExecution,
    ExecutionReceipt,
) {
    let mut c = h.connect().await;
    let a = authority();
    let (scope, registration) = external(h.root("scope", false), &a);

    let r = prepare(&c, scope, &registration, a, "lifecycle", mode, script);
    let receipt = launch(&mut c, &r).await;
    wait_for(&mut c, &r, &receipt, |_, out| out.contains("ready")).await;
    (c, r, receipt)
}

async fn wait_complete(h: &Harness, r: &PrepareExecution) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let value = journal(h, r);
        if value["output_end"].get("Complete").is_some() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "output did not complete: {value}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn drained_persisted_before_output_complete() {
    let h = Harness::start().await;
    let (mut c, r, receipt, writer) = escaped_output(&h, IoMode::Pipe).await;
    c.request(ExecutionRequest::Control(ExecutionControl {
        capability: capability(&r, &receipt, ExecutionControlPurpose::Interactive),
        action: ProcessAction::WriteStdin {
            bytes: b"done\n".to_vec(),
        },
    }))
    .await
    .unwrap();
    wait_for(&mut c, &r, &receipt, |state, out| {
        matches!(state, ExecutionState::Drained { .. }) && out.contains("tail")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        journal(&h, &r)["output_end"],
        "Open",
        "settlement must not invent output EOF"
    );
    // Observe acquires the same scope gate: it must remain usable while output is Open.
    tokio::time::timeout(Duration::from_millis(300), observe(&mut c, &r, &receipt))
        .await
        .unwrap();
    drop(writer);
    let value = wait_complete(&h, &r).await;
    assert!(value["phase"].get("Drained").is_some());
    assert_eq!(value["output_end"]["Complete"]["final_cursor"], 9);
    assert_eq!(
        std::fs::read(
            h.journal()
                .join(format!("{}.output", r.identity.execution_id.as_str()))
        )
        .unwrap(),
        b"readytail"
    );
}

#[tokio::test]
async fn output_complete_after_reader_eof_or_pty_eio() {
    for mode in [IoMode::Pipe, IoMode::Pty { cols: 80, rows: 24 }] {
        let h = Harness::start().await;
        let (mut c, r, receipt, writer) = escaped_output(&h, mode).await;
        c.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&r, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: b"done\n".to_vec(),
            },
        }))
        .await
        .unwrap();
        wait_for(&mut c, &r, &receipt, |state, _| {
            matches!(state, ExecutionState::Drained { .. })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(journal(&h, &r)["output_end"], "Open");
        drop(writer);
        wait_complete(&h, &r).await;
    }
}

#[tokio::test]
async fn stop_escalates_after_grace() {
    let h = Harness::start().await;
    let (mut c, r, receipt) = setup(&h, IoMode::Pipe, "trap '' TERM; printf ready; sleep 5").await;
    let stop: ProcessAction =
        serde_json::from_str("\"Stop\"").expect("lifecycle Stop action missing");
    let start = tokio::time::Instant::now();
    loop {
        assert!(matches!(
            c.request(ExecutionRequest::Control(ExecutionControl {
                capability: capability(&r, &receipt, ExecutionControlPurpose::Lifecycle),
                action: stop.clone(),
            }))
            .await
            .unwrap(),
            ExecutionReply::ActionComplete
        ));
        let (state, _) = observe(&mut c, &r, &receipt).await;
        if matches!(state, ExecutionState::Drained { .. }) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_millis(700),
            "repeated Stops postponed KILL"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "KILL preceded STOP_GRACE"
    );
    assert!(journal(&h, &r)["phase"].get("Drained").is_some());
}

#[tokio::test]
async fn graceful_shutdown_term_ignoring_child_persists_drained() {
    let mut h = Harness::start().await;
    let (_c, r, _) = setup(&h, IoMode::Pipe, "trap '' TERM; printf ready; sleep 5").await;
    let start = tokio::time::Instant::now();
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    loop {
        if h.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_millis(1800),
            "shutdown exceeded quiescence budget"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        journal(&h, &r)["phase"].get("Drained").is_some(),
        "shutdown returned without durable scope drain"
    );
}

#[tokio::test]
async fn host_reboot_is_no_child() {
    let mut h = Harness::start().await;
    let (mut c, r, receipt) = setup(&h, IoMode::Pipe, "printf ready; read -r release").await;
    // Keep the scope alive until its crash snapshot is captured, regardless of scheduling.
    // Seeing ready alone does not prove a timed child has not already drained.
    let active = journal(&h, &r);
    assert!(active["phase"].get("Spawned").is_some(), "{active}");
    assert_eq!(active["output_end"], "Open");
    assert_eq!(
        active["host_boot_id"],
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
    );
    // Release and drain the real scope before replaying its earlier nonterminal journal.
    assert!(matches!(
        c.request(ExecutionRequest::Control(ExecutionControl {
            capability: capability(&r, &receipt, ExecutionControlPurpose::Interactive),
            action: ProcessAction::WriteStdin {
                bytes: b"done\n".to_vec(),
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
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    h.child.wait().unwrap();
    let mut old = active;
    old["host_boot_id"] = "different-host-boot".into();
    std::fs::write(
        h.journal()
            .join(format!("{}.execution", r.identity.execution_id.as_str())),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let mut c = h.connect().await;
    assert_eq!(
        journal(&h, &r)["output_end"]["Incomplete"]["reason"],
        "host_rebooted"
    );
    let replay = c
        .request(ExecutionRequest::Launch(consume(&r)))
        .await
        .unwrap();
    assert!(
        matches!(&replay, ExecutionReply::Receipt { state: ExecutionState::NoChild { reason }, .. } if reason == "host_rebooted"),
        "reboot replay did not settle the nonterminal execution: {replay:?}"
    );
}

async fn escaped_output(
    h: &Harness,
    mode: IoMode,
) -> (
    RegisteredExecutorConnection,
    PrepareExecution,
    ExecutionReceipt,
    std::fs::File,
) {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixDatagram,
    };
    let path = h.temp.path().join("escape.sock");
    let socket = UnixDatagram::bind(&path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let script = format!(
        r#"exec python3 -c 'import socket,array,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM); s.connect("{}"); s.sendmsg([b"x"],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array("i",[1]))]); os.write(1,b"ready"); os.read(0,100); os.write(1,b"tail")'"#,
        path.display()
    );
    let (c, r, receipt) = setup(h, mode, &script).await;
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // Aligned ancillary buffer for exactly one transferred stdout descriptor.
    let mut ancillary = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = ancillary.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&ancillary);
    let file = unsafe {
        assert_eq!(
            libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC),
            1
        );
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        std::fs::File::from_raw_fd(std::ptr::read_unaligned(
            libc::CMSG_DATA(header).cast::<i32>(),
        ))
    };
    (c, r, receipt, file)
}

#[tokio::test]
async fn never_spawned_output_revoke_survives_restart() {
    let mut h = Harness::start().await;
    let mut c = h.connect().await;
    let a = authority();
    let (scope, registration) = external(h.root("never-spawned", false), &a);

    let r = prepare(
        &c,
        scope,
        &registration,
        a,
        "never-spawned",
        IoMode::Pipe,
        "exit 1",
    );
    submit(&mut c, r.clone()).await;
    c.request(ExecutionRequest::Revoke(consume(&r)))
        .await
        .unwrap();
    let end = journal(&h, &r)["output_end"].clone();
    assert_eq!(end, serde_json::json!({"Complete": {"final_cursor": 0}}));
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    h.child.wait().unwrap();
    drop(c);
    h.child = Harness::spawn(h.temp.path(), &h.socket);
    let _c = h.connect().await;
    assert_eq!(journal(&h, &r)["output_end"], end);
}

#[tokio::test]
async fn never_spawned_output_recovery_and_shutdown_paths() {
    for phase in [
        serde_json::json!("Prepared"),
        serde_json::json!({"Reserved": {"slot_version": 1}}),
    ] {
        let mut h = Harness::start().await;
        let mut c = h.connect().await;
        let a = authority();
        let (scope, registration) = external(h.root("no-child", false), &a);

        let r = prepare(
            &c,
            scope,
            &registration,
            a,
            "no-child",
            IoMode::Pipe,
            "exit 1",
        );
        submit(&mut c, r.clone()).await;
        let mut before = journal(&h, &r);
        unsafe {
            libc::kill(h.child.id() as i32, libc::SIGTERM);
        }
        h.child.wait().unwrap();
        assert_eq!(
            journal(&h, &r)["output_end"],
            serde_json::json!({"Complete":{"final_cursor":0}})
        );
        before["phase"] = phase;
        std::fs::write(
            h.journal()
                .join(format!("{}.execution", r.identity.execution_id.as_str())),
            serde_json::to_vec(&before).unwrap(),
        )
        .unwrap();
        drop(c);
        h.child = Harness::spawn(h.temp.path(), &h.socket);
        let _c = h.connect().await;
        assert_eq!(
            journal(&h, &r)["output_end"],
            serde_json::json!({"Complete":{"final_cursor":0}})
        );
    }
}
