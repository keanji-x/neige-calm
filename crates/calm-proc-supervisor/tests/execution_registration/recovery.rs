use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Pin while the supervisor still owns this live helper; never rediscover a PID after restart.
struct RunningScope(OwnedFd);
impl RunningScope {
    fn pin(receipt: &ExecutionReceipt) -> Self {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, receipt.process.pid, 0) };
        assert!(descriptor >= 0);
        Self(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
    }
    fn exited(&self, timeout: i32) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut descriptor, 1, timeout) > 0 }
    }
    fn stop(&self) -> bool {
        // The real helper's handler kills and collects its owned descendants before exiting.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGUSR1,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
        self.exited(5000)
    }
}
impl Drop for RunningScope {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[tokio::test]
async fn execution_restart_cannot_replace_active_source_incarnation_at_same_generation() {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("restart-source", true);
    let held = HeldSourceObjects::open(&root.path, true).unwrap();
    let marker = root.path.join("escaped");
    let grant = authority();
    let (scope, registration) = worktree(root, &grant);
    register(&mut client, registration).await;
    let original = client.registrations().last().unwrap().clone();
    let request = prepare(
        &client,
        scope.clone(),
        grant.clone(),
        "original",
        IoMode::Pipe,
        "printf original-ready; exec sleep 30",
    );
    let receipt = launch(&mut client, &request).await;
    let native = RunningScope::pin(&receipt);
    wait_for(&mut client, &request, &receipt, |_, output| {
        output.contains("original-ready")
    })
    .await;
    drop(client);
    harness.child.kill().unwrap();
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    let mut recovered = harness.connect().await;
    assert_ne!(recovered.executor_boot(), &original.transfer.executor_boot);
    assert!(matches!(
        recovered
            .request(ExecutionRequest::Launch(consume(&request)))
            .await
            .unwrap(),
        ExecutionReply::Receipt {
            state: ExecutionState::Unknown { .. },
            ..
        }
    ));
    assert!(!native.exited(0), "old helper must still be a live obligation");

    let mut replacement = original.transfer.clone();
    replacement.registration_id = ObjectRegistrationId::fresh().unwrap();
    replacement.executor_boot = recovered.executor_boot().clone();
    replacement.nonce = TransferNonce::fresh().unwrap();
    replacement.incarnation =
        SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap());
    let attached = recovered.attach(replacement, &held).await;
    let mut escaped = None;
    if attached.is_ok() {
        let next = prepare(
            &recovered,
            scope,
            grant,
            "another-alias",
            IoMode::Pipe,
            "printf escaped > escaped",
        );
        let prepared = recovered
            .request(ExecutionRequest::Prepare(next.clone()))
            .await
            .unwrap();
        let launched = recovered
            .request(ExecutionRequest::Launch(consume(&next)))
            .await
            .unwrap();
        if let ExecutionReply::Receipt {
            receipt: Some(ref actual),
            state: ExecutionState::Spawned,
        } = launched
        {
            wait_for(&mut recovered, &next, actual, |state, _| {
                matches!(state, ExecutionState::Drained { .. })
            })
            .await;
        }
        escaped = Some((prepared, launched));
    }
    assert!(native.stop(), "exact original helper did not exit after draining");
    assert!(
        attached.is_err(),
        "restart accepted a new source incarnation without recovery: {attached:?}; actual new execution: {escaped:?}; payload marker: {}",
        marker.exists()
    );
    assert!(!marker.exists());
}
