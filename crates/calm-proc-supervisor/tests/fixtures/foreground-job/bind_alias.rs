//! Disposable platform probe. It creates no mount visible to the parent namespace.
use std::{
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};
use workspace::execution::objects::{HeldObjectComparison, HeldSourceObjects};

fn key_hash(key: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut state = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut state);
    state.finish()
}

pub(super) fn run(root: &Path, alias: &Path) -> anyhow::Result<()> {
    // This executable starts before any runtime/worker threads; timeout is confined to this child.
    unsafe {
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(5);
    }
    anyhow::ensure!(
        unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } == 0,
        "private namespace unavailable: {}",
        std::io::Error::last_os_error()
    );
    // Stop propagation before the bind operation, even when the inherited mounts were shared.
    anyhow::ensure!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } == 0,
        "make private: {}",
        std::io::Error::last_os_error()
    );
    let source = std::ffi::CString::new(root.as_os_str().as_bytes())?;
    let target = std::ffi::CString::new(alias.as_os_str().as_bytes())?;
    anyhow::ensure!(
        unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } == 0,
        "bind owned directory: {}",
        std::io::Error::last_os_error()
    );
    let held = HeldSourceObjects::open(&root, false)?;
    let aliased = HeldSourceObjects::open(&alias, false)?;
    let left_stat = held.root().metadata()?;
    let right_stat = aliased.root().metadata()?;
    let left = held.live_identity()?;
    let right = aliased.live_identity()?;
    let cross_mount = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(cross_mount_rpc(&held, &aliased))?;
    println!(
        "{}",
        serde_json::json!({
            "same_physical_root":left_stat.dev()==right_stat.dev() && left_stat.ino()==right_stat.ino(),
            "different_mount":held.shape().objects[0].mount != aliased.shape().objects[0].mount,
            "same_bucket":left.root_key()==right.root_key(),
            "same_bucket_hash":key_hash(left.root_key())==key_hash(right.root_key()),
            "declared_different_root":left.compare(&right)==HeldObjectComparison::DifferentRoot,
            "comparison":format!("{:?}",left.compare(&right)),
            "cross_mount_rpc":format!("{cross_mount:?}"),
        })
    );
    Ok(())
}

/// Same real namespace/mount evidence, exercised through the actual registered socket endpoint.
async fn cross_mount_rpc(
    ancestor: &HeldSourceObjects,
    subject: &HeldSourceObjects,
) -> anyhow::Result<workspace::execution::objects::DirectoryRelationOutcome> {
    use calm_session::control::execution::*;
    use workspace::execution::objects::ObjectRole;
    let temporary = tempfile::tempdir()?;
    let socket = temporary.path().join("observer.sock");
    let secret = CapabilityToken::fresh()?;
    let registry = calm_proc_supervisor::ProcRegistry::new().with_executor(
        calm_proc_supervisor::execution::ExecutorConfig {
            journal_directory: temporary.path().join("journal"),
            helper_program: std::env::current_exe()?,
            kernel_client_secret: secret.clone(),
        },
    )?;
    let listener = calm_proc_supervisor::bind_control_listener(&socket)?;
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(calm_proc_supervisor::serve_with_listener(
        listener,
        socket.clone(),
        registry,
        receiver,
    ));
    let mut client = RegisteredExecutorConnection::connect(&socket, secret).await?;
    let mut receipts = Vec::new();
    for held in [ancestor, subject] {
        let metadata = held.root().metadata()?;
        let receipt = client
            .attach(
                AttachRegistration {
                    registration_id: ObjectRegistrationId::fresh()?,
                    executor_boot: client.executor_boot().clone(),
                    nonce: TransferNonce::fresh()?,
                    incarnation: SourceObjectIncarnation::ExternalDir(
                        ExternalDirectoryIncarnationId::fresh()?,
                    ),
                    registration: ExecutionRegistration::ExternalDir {
                        directory_id: ExternalDirectoryId::fresh()?,
                        root: DirectoryRegistration {
                            path: held.shape().root.clone(),
                            device: metadata.dev(),
                            inode: metadata.ino(),
                        },
                        registration_version: 9,
                        admission_open: true,
                        authority: ExecutionAuthority {
                            grant_id: ExecutionGrantId::fresh()?,
                            authorization_binding: AuthorizationBindingId::fresh()?,
                            authorization_version: 3,
                        },
                    },
                    shape: held.shape().clone(),
                },
                held,
            )
            .await?;
        receipts.push(RegisteredDirectoryRoot {
            receipt,
            role: ObjectRole::Root,
        });
    }
    let observation = client
        .observe_directories(DirectoryRelationRequest {
            executor_boot: client.executor_boot().clone(),
            nonce: TransferNonce::fresh()?,
            ancestor: receipts[0].clone(),
            subject: receipts[1].clone(),
        })
        .await?;
    let outcome = observation.outcome().clone();
    let _ = shutdown.send(());
    task.await??;
    Ok(outcome)
}
