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
        (
            ExecutionRegistration::Worktree { generation, .. },
            ExecutionRegistration::Worktree {
                generation: next, ..
            },
        ) => next > generation,
        (
            ExecutionRegistration::ExternalDir {
                registration_version,
                ..
            },
            ExecutionRegistration::ExternalDir {
                registration_version: next,
                ..
            },
        ) => next > registration_version,
        _ => false,
    }
}

pub(super) fn registration_successor(
    previous: &ExecutionRegistration,
    next: &ExecutionRegistration,
) -> bool {
    if previous == next {
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
                authority,
                ..
            },
            ExecutionRegistration::Worktree {
                binding_version: new_version,
                generation: new_generation,
                install_epoch: new_epoch,
                admission_open: open,
                authority: grant,
                ..
            },
        ) => {
            binding_version == new_version
                && admission_open == open
                && authority == grant
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
pub(super) fn same_runtime(scope: &ExecutionScope, registration: &ExecutionRegistration) -> bool {
    match (scope, registration) {
        (
            ExecutionScope::Worktree {
                generation,
                install_epoch,
                ..
            },
            ExecutionRegistration::Worktree {
                generation: actual,
                install_epoch: epoch,
                ..
            },
        ) => generation == actual && install_epoch == epoch,
        (
            ExecutionScope::ExternalDir { directory_id, .. },
            ExecutionRegistration::ExternalDir {
                directory_id: actual,
                ..
            },
        ) => directory_id == actual,
        _ => false,
    }
}
pub(super) fn registration_matches(
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
                authority,
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
                && authority == &identity.authority
                && (!require_admission || *admission_open)
        }
        (
            ExecutionRegistration::ExternalDir {
                directory_id,
                registration_version,
                admission_open,
                authority,
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
                && authority == &identity.authority
                && (!require_admission || *admission_open)
        }
        _ => false,
    }
}
