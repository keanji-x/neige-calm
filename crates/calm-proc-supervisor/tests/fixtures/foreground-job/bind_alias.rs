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

pub(super) fn enter_private_namespace() -> anyhow::Result<()> {
    // This executable starts before any runtime/worker threads; timeout is confined to this child.
    unsafe {
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(15);
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
    Ok(())
}

pub(super) fn run(root: &Path, alias: &Path) -> anyhow::Result<()> {
    enter_private_namespace()?;
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
    let held = HeldSourceObjects::open(root, false)?;
    let aliased = HeldSourceObjects::open(alias, false)?;
    let left_stat = held.root().metadata()?;
    let right_stat = aliased.root().metadata()?;
    let left = held.live_identity()?;
    let right = aliased.live_identity()?;
    println!(
        "{}",
        serde_json::json!({
            "same_physical_root":left_stat.dev()==right_stat.dev() && left_stat.ino()==right_stat.ino(),
            "different_mount":held.shape().objects[0].mount != aliased.shape().objects[0].mount,
            "same_bucket":left.root_key()==right.root_key(),
            "same_bucket_hash":key_hash(left.root_key())==key_hash(right.root_key()),
            "declared_different_root":left.compare(&right)==HeldObjectComparison::DifferentRoot,
            "comparison":format!("{:?}",left.compare(&right)),
        })
    );
    Ok(())
}
