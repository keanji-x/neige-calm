//! Qualification for two actual directory FDs, without an execution/source registration.
use super::*;
use crate::execution::objects::DirectoryPairFacts;
use std::os::fd::{AsFd, BorrowedFd};

/// Continuous object/profile evidence only. An ancestry result still needs the caller's
/// actual owner exclusion and stability conditions at consumption. No serde/scalar constructor.
pub struct QualifiedDirectoryPair<'files> {
    directories: [&'files File; 2],
    namespace: NamespaceContext,
    facts: DirectoryPairFacts,
    qualified: [QualifiedObjectFact; 2],
    mounts: [MountRecord; 2],
}

pub fn qualify_directory_pair<'files>(
    ancestor: &'files File,
    subject: &'files File,
) -> Result<QualifiedDirectoryPair<'files>> {
    local_object_platform().map_err(ObjectQualificationFailure::Unsupported)?;
    let namespace = NamespaceContext::capture()
        .map_err(|error| unknown_io(ObjectQualificationStage::Namespace, error))?;
    let mount_namespace = namespace
        .inode()
        .map_err(|error| unknown_io(ObjectQualificationStage::Namespace, error))?;
    let table = MountTable::read()?;
    let (ancestor_fact, ancestor_mount) = qualify_file(ancestor, ObjectRole::Root, &table)?;
    let (subject_fact, subject_mount) = qualify_file(subject, ObjectRole::Root, &table)?;
    if ancestor_fact.object.mount != subject_fact.object.mount {
        return Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::DirectoryMountBoundary {
                ancestor: ancestor_fact.object.mount,
                subject: subject_fact.object.mount,
            },
        ));
    }
    let pair = QualifiedDirectoryPair {
        directories: [ancestor, subject],
        namespace,
        facts: DirectoryPairFacts {
            mount_namespace,
            ancestor: ancestor_fact.object.clone(),
            subject: subject_fact.object.clone(),
        },
        qualified: [ancestor_fact, subject_fact],
        mounts: [ancestor_mount, subject_mount],
    };
    pair.check_context()?;
    Ok(pair)
}

impl QualifiedDirectoryPair<'_> {
    pub fn facts(&self) -> &DirectoryPairFacts {
        &self.facts
    }
    pub fn qualified_objects(&self) -> &[QualifiedObjectFact; 2] {
        &self.qualified
    }
    pub fn directories(&self) -> [&File; 2] {
        self.directories
    }
    pub fn descriptors(&self) -> [BorrowedFd<'_>; 2] {
        [self.directories[0].as_fd(), self.directories[1].as_fd()]
    }
    /// Rechecks object/profile continuity; this does not renew an ancestry observation.
    pub fn revalidate(&self) -> Result<()> {
        self.check_context()?;
        let current = qualify_directory_pair(self.directories[0], self.directories[1])?;
        if current.facts != self.facts || current.qualified != self.qualified {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::DirectoryPairChanged,
            ));
        }
        if current.mounts != self.mounts {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::MountInformationChanged {
                    mount: self.facts.ancestor.mount,
                },
            ));
        }
        Ok(())
    }
    fn check_context(&self) -> Result<()> {
        if !self
            .namespace
            .matches_current()
            .map_err(|error| unknown_io(ObjectQualificationStage::Namespace, error))?
        {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::NamespaceChanged,
            ));
        }
        let table = MountTable::read()?;
        for (object, expected) in self.qualified.iter().zip(&self.mounts) {
            if table.get(object.object.mount)? != expected {
                return Err(ObjectQualificationFailure::Unknown(
                    ObjectQualificationUnknown::MountInformationChanged {
                        mount: object.object.mount,
                    },
                ));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn object_qualification_pair_retains_real_directory_objects_without_named_root_assumptions() {
        let temp = tempfile::tempdir().unwrap();
        let ancestor_path = temp.path().join("ancestor");
        let subject_path = temp.path().join("subject");
        std::fs::create_dir(&ancestor_path).unwrap();
        std::fs::create_dir(&subject_path).unwrap();
        let ancestor = File::open(&ancestor_path).unwrap();
        let subject = File::open(&subject_path).unwrap();
        let pair = qualify_directory_pair(&ancestor, &subject).unwrap();
        assert!(
            pair.qualified_objects()
                .iter()
                .all(|fact| fact.profile == LocalObjectProfile::Ext4DriverHeldV1)
        );
        let original = pair.facts().clone();
        std::fs::rename(&subject_path, ancestor_path.join("moved")).unwrap();
        std::fs::create_dir(&subject_path).unwrap();
        pair.revalidate().unwrap();
        assert_eq!(pair.facts(), &original);
        let replacement = File::open(&subject_path).unwrap();
        let new_pair = qualify_directory_pair(&ancestor, &replacement).unwrap();
        assert_ne!(
            new_pair.facts().subject,
            original.subject,
            "a pathname must not replace the retained subject FD"
        );
        std::fs::remove_dir(ancestor_path.join("moved")).unwrap();
        assert!(
            pair.revalidate().is_err(),
            "a detached original object retained a qualified witness"
        );
    }

    #[test]
    fn object_qualification_pair_refuses_unqualified_filesystem_and_non_directory() {
        let tmpfs = tempfile::tempdir_in("/dev/shm").unwrap();
        let directory = File::open(tmpfs.path()).unwrap();
        assert!(matches!(
            qualify_directory_pair(&directory, &directory),
            Err(ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::Filesystem { .. }
            ))
        ));
        let regular = tempfile::tempfile().unwrap();
        assert!(matches!(
            qualify_directory_pair(&regular, &regular),
            Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::ObjectChanged(ObjectRole::Root)
            ))
        ));
    }
}
