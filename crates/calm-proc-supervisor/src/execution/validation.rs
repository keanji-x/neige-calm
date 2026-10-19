use super::ResourceKey;
use calm_session::control::execution::*;

pub(super) fn older_runtime(
    incoming: &ExecutionRegistration,
    known: &ExecutionRegistration,
) -> bool {
    match (incoming, known) {
        (
            ExecutionRegistration::Worktree {
                generation,
                install_epoch,
                ..
            },
            ExecutionRegistration::Worktree {
                generation: old_generation,
                install_epoch: old_epoch,
                ..
            },
        ) => generation < old_generation || install_epoch < old_epoch,
        (
            ExecutionRegistration::ExternalDir {
                registration_version,
                ..
            },
            ExecutionRegistration::ExternalDir {
                registration_version: previous,
                ..
            },
        ) => registration_version < previous,
        _ => true,
    }
}

pub(super) fn same_directory(left: &ExecutionRegistration, right: &ExecutionRegistration) -> bool {
    let root = |registration: &ExecutionRegistration| match registration {
        ExecutionRegistration::Worktree { root, .. }
        | ExecutionRegistration::ExternalDir { root, .. } => (root.device, root.inode),
    };
    root(left) == root(right)
}

pub(super) fn directory_recovery_version(
    previous: &ExecutionRegistration,
    next: &ExecutionRegistration,
) -> bool {
    match (previous, next) {
        (ExecutionRegistration::Worktree { .. }, ExecutionRegistration::Worktree { .. })
        | (ExecutionRegistration::ExternalDir { .. }, ExecutionRegistration::ExternalDir { .. }) => {
            source_recovery_version(next) > source_recovery_version(previous)
        }
        _ => false,
    }
}

pub(super) fn source_recovery_version(registration: &ExecutionRegistration) -> u64 {
    match registration {
        ExecutionRegistration::Worktree { generation, .. } => *generation,
        ExecutionRegistration::ExternalDir {
            registration_version,
            ..
        } => *registration_version,
    }
}

/// Source continuity excludes exchange/boot IDs and per-operation grants, which may differ.
pub(super) fn same_source(previous: &AttachRegistration, next: &AttachRegistration) -> bool {
    same_directory(&previous.registration, &next.registration)
        && previous.incarnation == next.incarnation
        && previous.shape == next.shape
}

/// One source-successor rule for loaded receipt history and new per-binding attachments.
pub(super) fn source_successor(previous: &AttachRegistration, next: &AttachRegistration) -> bool {
    registration_successor(&previous.registration, &next.registration)
        && (same_source(previous, next)
            || directory_recovery_version(&previous.registration, &next.registration))
}

/// A per-binding lifecycle projection; accepted per-operation grants are deliberately absent.
#[derive(PartialEq, Eq)]
enum LifecycleFence<'a> {
    Worktree {
        workspace: &'a workspace::identity::WorkspaceId,
        root: &'a DirectoryRegistration,
        binding: &'a WorkspaceBindingId,
        binding_version: u64,
        generation: u64,
        epoch: u64,
        open: bool,
    },
    ExternalDir {
        directory: &'a ExternalDirectoryId,
        root: &'a DirectoryRegistration,
        version: u64,
        open: bool,
    },
}
fn lifecycle_fence(registration: &ExecutionRegistration) -> LifecycleFence<'_> {
    match registration {
        ExecutionRegistration::Worktree {
            workspace_id,
            root,
            binding_id,
            binding_version,
            generation,
            install_epoch,
            admission_open,
            ..
        } => LifecycleFence::Worktree {
            workspace: workspace_id,
            root,
            binding: binding_id,
            binding_version: *binding_version,
            generation: *generation,
            epoch: *install_epoch,
            open: *admission_open,
        },
        ExecutionRegistration::ExternalDir {
            directory_id,
            root,
            registration_version,
            admission_open,
            ..
        } => LifecycleFence::ExternalDir {
            directory: directory_id,
            root,
            version: *registration_version,
            open: *admission_open,
        },
    }
}

pub(super) fn registration_successor(
    previous: &ExecutionRegistration,
    next: &ExecutionRegistration,
) -> bool {
    if lifecycle_fence(previous) == lifecycle_fence(next) {
        return true;
    }
    if registration_version(next) > registration_version(previous) && !older_runtime(next, previous)
    {
        return true;
    }
    match (previous, next) {
        (
            ExecutionRegistration::Worktree {
                binding_version,
                generation,
                install_epoch,
                admission_open,
                ..
            },
            ExecutionRegistration::Worktree {
                binding_version: new_version,
                generation: new_generation,
                install_epoch: new_epoch,
                admission_open: open,
                ..
            },
        ) => {
            binding_version == new_version
                && admission_open == open
                && new_generation >= generation
                && new_epoch >= install_epoch
                && (new_generation > generation || new_epoch > install_epoch)
        }
        _ => false,
    }
}

pub(super) fn scope_resource(scope: &ExecutionScope) -> ResourceKey {
    match scope {
        ExecutionScope::Worktree { workspace_id, .. } => {
            ResourceKey::Worktree(workspace_id.as_str().into())
        }
        ExecutionScope::ExternalDir { directory_id, .. } => {
            ResourceKey::External(directory_id.as_str().into())
        }
    }
}
pub(super) fn registration_resource(registration: &ExecutionRegistration) -> ResourceKey {
    match registration {
        ExecutionRegistration::Worktree { workspace_id, .. } => {
            ResourceKey::Worktree(workspace_id.as_str().into())
        }
        ExecutionRegistration::ExternalDir { directory_id, .. } => {
            ResourceKey::External(directory_id.as_str().into())
        }
    }
}
pub(super) fn registration_key(registration: &ExecutionRegistration) -> String {
    match registration {
        ExecutionRegistration::Worktree {
            workspace_id,
            binding_id,
            ..
        } => format!("w-{}-{}", workspace_id.as_str(), binding_id.as_str()),
        ExecutionRegistration::ExternalDir { directory_id, .. } => {
            format!("e-{}", directory_id.as_str())
        }
    }
}
pub(super) fn scope_registration_key(scope: &ExecutionScope) -> String {
    match scope {
        ExecutionScope::Worktree {
            workspace_id,
            binding_id,
            ..
        } => format!("w-{}-{}", workspace_id.as_str(), binding_id.as_str()),
        ExecutionScope::ExternalDir { directory_id, .. } => format!("e-{}", directory_id.as_str()),
    }
}
pub(super) fn scope_cwd(scope: &ExecutionScope) -> &workspace::identity::WorkspaceCwd {
    match scope {
        ExecutionScope::Worktree { cwd, .. } | ExecutionScope::ExternalDir { cwd, .. } => cwd,
    }
}
pub(super) fn registration_version(registration: &ExecutionRegistration) -> u64 {
    match registration {
        ExecutionRegistration::Worktree {
            binding_version, ..
        } => *binding_version,
        ExecutionRegistration::ExternalDir {
            registration_version,
            ..
        } => *registration_version,
    }
}
pub(super) fn registration_matches(
    registration: &ExecutionRegistration,
    identity: &ExecutionIdentity,
    require_admission: bool,
) -> bool {
    let authority = match registration {
        ExecutionRegistration::Worktree { authority, .. }
        | ExecutionRegistration::ExternalDir { authority, .. } => authority,
    };
    authority == &identity.authority && lifecycle_matches(registration, identity, require_admission)
}

pub(super) fn lifecycle_matches(
    registration: &ExecutionRegistration,
    identity: &ExecutionIdentity,
    require_admission: bool,
) -> bool {
    match (registration, &identity.scope) {
        (
            ExecutionRegistration::Worktree {
                workspace_id,
                binding_id,
                binding_version,
                generation,
                install_epoch,
                admission_open,
                ..
            },
            ExecutionScope::Worktree {
                workspace_id: expected,
                binding_id: binding,
                binding_version: version,
                generation: expected_generation,
                install_epoch: expected_epoch,
                ..
            },
        ) => {
            workspace_id == expected
                && binding_id == binding
                && binding_version == version
                && generation == expected_generation
                && install_epoch == expected_epoch
                && (!require_admission || *admission_open)
        }
        (
            ExecutionRegistration::ExternalDir {
                directory_id,
                registration_version,
                admission_open,
                ..
            },
            ExecutionScope::ExternalDir {
                directory_id: expected,
                registration_version: version,
                ..
            },
        ) => {
            directory_id == expected
                && registration_version == version
                && (!require_admission || *admission_open)
        }
        _ => false,
    }
}
