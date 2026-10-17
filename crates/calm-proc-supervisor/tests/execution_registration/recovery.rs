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
    active_source_replacement(Replacement::WorktreeIncarnation).await;
}

#[tokio::test]
async fn execution_restart_cannot_replace_active_external_incarnation_at_same_version() {
    active_source_replacement(Replacement::ExternalIncarnation).await;
}

#[tokio::test]
async fn execution_restart_cannot_replace_active_worktree_layout_at_same_generation() {
    active_source_replacement(Replacement::WorktreeLayout).await;
}

#[derive(Clone, Copy)]
enum Replacement {
    WorktreeIncarnation,
    ExternalIncarnation,
    WorktreeLayout,
}

fn new_exchange(
    client: &RegisteredExecutorConnection,
    previous: &AttachRegistration,
) -> AttachRegistration {
    let mut transfer = previous.clone();
    transfer.registration_id = ObjectRegistrationId::fresh().unwrap();
    transfer.executor_boot = client.executor_boot().clone();
    transfer.nonce = TransferNonce::fresh().unwrap();
    transfer
}

fn next_incarnation(previous: &SourceObjectIncarnation) -> SourceObjectIncarnation {
    match previous {
        SourceObjectIncarnation::Worktree(_) => {
            SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap())
        }
        SourceObjectIncarnation::ExternalDir(_) => {
            SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh().unwrap())
        }
    }
}

async fn restart(harness: &mut Harness) -> RegisteredExecutorConnection {
    harness.child.kill().unwrap();
    harness.child.wait().unwrap();
    harness.child = Harness::spawn(harness.temp.path(), &harness.socket);
    harness.connect().await
}

async fn active_source_replacement(replacement_kind: Replacement) {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let worktree_source = !matches!(replacement_kind, Replacement::ExternalIncarnation);
    let root = harness.root("restart-source", worktree_source);
    let held = HeldSourceObjects::open(&root.path, worktree_source).unwrap();
    let marker = root.path.join("escaped");
    let grant = authority();
    let (scope, registration) = if worktree_source {
        worktree(root, &grant)
    } else {
        external(root, &grant)
    };
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
    let mut recovered = restart(&mut harness).await;
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
    assert!(
        !native.exited(0),
        "old helper must still be a live obligation"
    );

    let mut replacement = new_exchange(&recovered, &original.transfer);
    let replacement_held = if matches!(replacement_kind, Replacement::WorktreeLayout) {
        let root = &held.shape().root;
        std::fs::rename(root.join(".git"), root.join("original-git")).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(root)
                .status()
                .unwrap()
                .success()
        );
        let changed = HeldSourceObjects::open(root, true).unwrap();
        assert_ne!(changed.shape(), held.shape());
        changed
    } else {
        replacement.incarnation = next_incarnation(&replacement.incarnation);
        held.try_clone().unwrap()
    };
    replacement.shape = replacement_held.shape().clone();
    let attached = recovered.attach(replacement, &replacement_held).await;
    let mut escaped = None;
    if attached.is_ok() {
        let next = prepare(
            &recovered,
            scope.clone(),
            grant.clone(),
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
    if attached.is_err() && !matches!(replacement_kind, Replacement::WorktreeLayout) {
        // Old uncertainty does not veto actual continuity of the original source.
        recovered
            .attach(new_exchange(&recovered, &original.transfer), &held)
            .await
            .unwrap();
        let continued = prepare(
            &recovered,
            scope,
            grant,
            "same-source-continuation",
            IoMode::Pipe,
            "printf continued",
        );
        let current = launch(&mut recovered, &continued).await;
        wait_for(&mut recovered, &continued, &current, |state, output| {
            matches!(state, ExecutionState::Drained { .. }) && output.contains("continued")
        })
        .await;
    }
    assert!(
        native.stop(),
        "exact original helper did not exit after draining"
    );
    assert!(
        attached.is_err(),
        "restart accepted a new source incarnation without recovery: {attached:?}; actual new execution: {escaped:?}; payload marker: {}",
        marker.exists()
    );
    assert!(!marker.exists());
}

fn advance_source(scope: &mut ExecutionScope, registration: &mut ExecutionRegistration) {
    match (scope, registration) {
        (
            ExecutionScope::Worktree { generation, .. },
            ExecutionRegistration::Worktree {
                generation: version,
                ..
            },
        ) => {
            *generation += 1;
            *version += 1;
        }
        (
            ExecutionScope::ExternalDir {
                registration_version,
                ..
            },
            ExecutionRegistration::ExternalDir {
                registration_version: version,
                ..
            },
        ) => {
            *registration_version += 1;
            *version += 1;
        }
        _ => panic!("fixture resource kinds differ"),
    }
}

fn new_binding(scope: &mut ExecutionScope, registration: &mut ExecutionRegistration) {
    let next = WorkspaceBindingId::fresh().unwrap();
    let (
        ExecutionScope::Worktree { binding_id, .. },
        ExecutionRegistration::Worktree {
            binding_id: registered,
            ..
        },
    ) = (scope, registration)
    else {
        panic!("fixture requires worktree bindings");
    };
    *binding_id = next.clone();
    *registered = next;
}

#[tokio::test]
async fn execution_restart_settled_worktree_recovery_preserves_current_multibinding_continuation() {
    settled_source_recovery(true).await;
}

#[tokio::test]
async fn execution_restart_settled_external_recovery_preserves_current_continuation() {
    settled_source_recovery(false).await;
}

async fn settled_source_recovery(worktree_source: bool) {
    let mut harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("settled-source", worktree_source);
    let held = HeldSourceObjects::open(&root.path, worktree_source).unwrap();
    let grant = authority();
    let (mut scope, registration) = if worktree_source {
        worktree(root, &grant)
    } else {
        external(root, &grant)
    };
    register(&mut client, registration).await;
    let original = client.registrations().last().unwrap().clone();
    let request = prepare(
        &client,
        scope.clone(),
        grant.clone(),
        "settled",
        IoMode::Pipe,
        "printf settled",
    );
    let first = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &first, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(client);
    let mut recovered = restart(&mut harness).await;
    let mut next = new_exchange(&recovered, &original.transfer);
    next.incarnation = next_incarnation(&next.incarnation);
    assert!(
        recovered.attach(next.clone(), &held).await.is_err(),
        "settled history still requires an explicit source recovery version"
    );
    advance_source(&mut scope, &mut next.registration);
    if worktree_source {
        new_binding(&mut scope, &mut next.registration);
    }
    let current_source = recovered.attach(next, &held).await.unwrap();
    let current = prepare(
        &recovered,
        scope.clone(),
        grant.clone(),
        "recovered",
        IoMode::Pipe,
        "printf recovered",
    );
    let native = launch(&mut recovered, &current).await;
    wait_for(&mut recovered, &current, &native, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
    drop(recovered);

    let mut resumed = restart(&mut harness).await;
    let resumed_source = resumed
        .attach(new_exchange(&resumed, &current_source.transfer), &held)
        .await
        .unwrap();
    assert_eq!(
        resumed_source.transfer.incarnation,
        current_source.transfer.incarnation
    );
    assert!(matches!(
        resumed.request(ExecutionRequest::Launch(consume(&request))).await.unwrap(),
        ExecutionReply::Receipt { state: ExecutionState::Drained { .. }, receipt: Some(ref old) } if *old == first
    ));
    let active = prepare(
        &resumed,
        scope.clone(),
        grant.clone(),
        "active-current",
        IoMode::Pipe,
        "printf current-ready; exec sleep 30",
    );
    let active_receipt = launch(&mut resumed, &active).await;
    let active_native = RunningScope::pin(&active_receipt);
    wait_for(&mut resumed, &active, &active_receipt, |_, output| {
        output.contains("current-ready")
    })
    .await;
    let mut peer_transfer = new_exchange(&resumed, &resumed_source.transfer);
    if worktree_source {
        new_binding(&mut scope, &mut peer_transfer.registration);
    }
    let peer_grant = authority();
    match &mut peer_transfer.registration {
        ExecutionRegistration::Worktree { authority, .. }
        | ExecutionRegistration::ExternalDir { authority, .. } => *authority = peer_grant.clone(),
    }
    resumed.attach(peer_transfer, &held).await.unwrap();
    let peer = prepare(
        &resumed,
        scope,
        peer_grant,
        "peer-current",
        IoMode::Pipe,
        "printf peer",
    );
    let peer_receipt = launch(&mut resumed, &peer).await;
    wait_for(&mut resumed, &peer, &peer_receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("peer")
    })
    .await;
    assert!(
        !active_native.exited(0),
        "another binding/grant closed the current peer"
    );
    assert!(active_native.stop());
    wait_for(&mut resumed, &active, &active_receipt, |state, _| {
        matches!(state, ExecutionState::Drained { .. })
    })
    .await;
}

#[tokio::test]
async fn execution_attachment_failed_persistence_keeps_exact_source_claim_recoverable() {
    let harness = Harness::start().await;
    let mut client = harness.connect().await;
    let root = harness.root("pending-source", true);
    let held = HeldSourceObjects::open(&root.path, true).unwrap();
    let grant = authority();
    let (scope, registration) = worktree(root, &grant);
    let original = AttachRegistration {
        registration_id: ObjectRegistrationId::fresh().unwrap(),
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh().unwrap(),
        incarnation: SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh().unwrap()),
        registration,
        shape: held.shape().clone(),
    };
    let obstruction = harness.journal().join(format!(
        "{}.registration",
        original.registration_id.as_str()
    ));
    std::fs::create_dir(&obstruction).unwrap();
    assert!(client.attach(original.clone(), &held).await.is_err());
    assert!(client.registrations().is_empty());
    let mut conflicting = new_exchange(&client, &original);
    conflicting.incarnation = next_incarnation(&original.incarnation);
    assert!(
        client.attach(conflicting, &held).await.is_err(),
        "failed persistence lost the reserved source claim"
    );
    std::fs::remove_dir(obstruction).unwrap();
    let acknowledged = client.attach(original.clone(), &held).await.unwrap();
    assert_eq!(acknowledged.transfer, original);
    let request = prepare(
        &client,
        scope,
        grant,
        "retried-source",
        IoMode::Pipe,
        "printf retried",
    );
    let receipt = launch(&mut client, &request).await;
    wait_for(&mut client, &request, &receipt, |state, output| {
        matches!(state, ExecutionState::Drained { .. }) && output.contains("retried")
    })
    .await;
}
