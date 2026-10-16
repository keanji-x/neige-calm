//! Current held-directory facts. These types never authorize execution or future path confinement.
use super::{ContainmentKind, ContainmentUnknown, ContainmentUnsupported, ObjectFact, ObjectRole};
use serde::{Deserialize, Serialize};
use std::{fs::File, os::unix::fs::MetadataExt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryPairFacts {
    pub mount_namespace: u64,
    pub ancestor: ObjectFact,
    pub subject: ObjectFact,
}

pub enum DirectoryPairFailure {
    Unknown(ContainmentUnknown),
    Unsupported(ContainmentUnsupported),
}

/// IO must occur outside the canonical-index lock. Both original descriptors stay held.
pub fn current_directory_pair(
    ancestor: &File,
    subject: &File,
) -> Result<DirectoryPairFacts, DirectoryPairFailure> {
    use DirectoryPairFailure::{Unknown, Unsupported};
    if !cfg!(target_os = "linux") {
        return Err(Unsupported(ContainmentUnsupported::Platform));
    }
    let mount_namespace =
        super::namespace().map_err(|error| Unknown(ContainmentUnknown::ObjectMetadata(error)))?;
    let mut facts = Vec::with_capacity(2);
    for root in [ancestor, subject] {
        let metadata = root
            .metadata()
            .map_err(|error| Unknown(ContainmentUnknown::ObjectMetadata(error)))?;
        if !metadata.is_dir() || metadata.nlink() == 0 {
            return Err(Unknown(ContainmentUnknown::DetachedDirectory));
        }
        facts.push(
            super::fact(root, ObjectRole::Root)
                .map_err(|error| Unknown(ContainmentUnknown::ObjectMetadata(error)))?,
        );
    }
    if facts[0].mount != facts[1].mount {
        return Err(Unsupported(ContainmentUnsupported::DifferentMount));
    }
    Ok(DirectoryPairFacts {
        mount_namespace,
        ancestor: facts.remove(0),
        subject: facts.remove(0),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryObservationStage {
    Namespace,
    AncestorCwd,
    SubjectCwd,
    GetCwd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryRelationUnknown {
    ObjectsChanged,
    DetachedDirectory,
    HelperSpawn,
    HelperIo,
    HelperProtocol,
    HelperExited,
    Timeout,
    SystemCall {
        stage: DirectoryObservationStage,
        errno: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryRelationUnsupported {
    Platform,
    DifferentMount,
    UserNamespace { errno: i32 },
    RootChange { errno: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryRelationOutcome {
    Contains(ContainmentKind),
    /// Subject was outside ancestor at this observation; this grants no future rename guarantee.
    Disjoint,
    Unknown(DirectoryRelationUnknown),
    Unsupported(DirectoryRelationUnsupported),
}
