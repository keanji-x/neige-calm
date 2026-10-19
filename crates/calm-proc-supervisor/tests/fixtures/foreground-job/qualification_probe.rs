//! Real qualified APIs, confined to this disposable child namespace.
use std::{os::unix::ffi::OsStrExt, path::Path};
use workspace::{execution::objects::*, identity::WorkspaceCwd};

pub(super) fn run(root: &Path, source: &Path, _helper: &Path) -> anyhow::Result<()> {
    let original = HeldSourceObjects::open(root, true)?;
    let original_qualified = original.qualify_local()?;
    let original_identity = original.live_identity()?;
    let original_pair = qualify_directory_pair(original.root(), original.root())?;
    super::bind_alias::enter_private_namespace()?;
    anyhow::ensure!(
        matches!(
            original_pair.revalidate(),
            Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::NamespaceChanged
            ))
        ),
        "one-shot pair survived an actual namespace change"
    );
    let namespace_changed_unknown = matches!(
        original_qualified.revalidate(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::NamespaceChanged
        ))
    );
    let current = HeldSourceObjects::open(root, true)?;
    let namespace_comparison_unsupported = original_identity.compare(&current.live_identity()?)
        == HeldObjectComparison::NamespaceUnsupported;
    let mut result = object_boundary(root, source, &current)?;
    result["namespace_changed_unknown"] = namespace_changed_unknown.into();
    result["namespace_comparison_unsupported"] = namespace_comparison_unsupported.into();
    println!("{result}");
    Ok(())
}

fn object_boundary(
    root: &Path,
    source: &Path,
    held: &HeldSourceObjects,
) -> anyhow::Result<serde_json::Value> {
    let target = root.join("mounted");
    let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    anyhow::ensure!(
        unsafe {
            libc::mount(
                source_c.as_ptr(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } == 0,
        "private bind: {}",
        std::io::Error::last_os_error()
    );
    let cwd_cross_mount_unsupported = matches!(
        held.qualify_local()?.open_cwd(
            &WorkspaceCwd::try_from("mounted".to_owned()).map_err(|e| anyhow::anyhow!(e))?
        ),
        Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::CwdMountBoundary { .. }
        ))
    );
    let bound = HeldSourceObjects::open(&target, false)?;
    let qualified = bound.qualify_local()?;
    anyhow::ensure!(
        matches!(
            qualify_directory_pair(held.root(), bound.root()),
            Err(ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::DirectoryMountBoundary { .. }
            ))
        ),
        "one-shot pair accepted a real bind boundary"
    );
    anyhow::ensure!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        } == 0,
        "private readonly remount: {}",
        std::io::Error::last_os_error()
    );
    let changed_mount_unknown = matches!(
        qualified.revalidate(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::MountInformationChanged { .. }
        ))
    );
    anyhow::ensure!(
        unsafe { libc::umount2(target_c.as_ptr(), libc::MNT_DETACH) } == 0,
        "detach private bind: {}",
        std::io::Error::last_os_error()
    );
    let lost_mount_unknown = matches!(
        bound.qualify_local(),
        Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::MountInformationMissing { .. }
        ))
    );
    let no_payload = !source.join("forbidden").exists() && !target.join("forbidden").exists();
    Ok(
        serde_json::json!({ "cwd_cross_mount_unsupported": cwd_cross_mount_unsupported, "changed_mount_unknown": changed_mount_unknown, "lost_mount_unknown": lost_mount_unknown, "no_payload": no_payload }),
    )
}
