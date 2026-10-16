//! Local held-object semantics, not authorization, Ready, ancestry or backing independence.
use super::{HeldObjectIdentity, HeldRootKey, HeldSourceObjects, ObjectFact, ObjectRole, fact};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::File, io, os::unix::fs::MetadataExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalObjectProfile {
    /// Linux ext4 driver inode lifetime, actual FD FS/mount checks and continuous retention.
    Ext4DriverHeldV1,
}

/// Diagnostic facts are not a constructor for QualifiedHeldObjects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualifiedObjectFact {
    pub object: ObjectFact,
    pub profile: LocalObjectProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectQualificationUnsupported {
    Platform,
    Filesystem {
        role: ObjectRole,
        magic: u64,
        filesystem: String,
    },
    CwdMountBoundary {
        root: u64,
        cwd: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectQualificationStage {
    Namespace,
    Object(ObjectRole),
    Filesystem(ObjectRole),
    MountInformation,
    Cwd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectQualificationUnknown {
    Io {
        stage: ObjectQualificationStage,
        detail: String,
    },
    NamespaceChanged,
    RoleClosureChanged,
    ObjectChanged(ObjectRole),
    MountInformationMissing {
        mount: u64,
    },
    MountInformationChanged {
        mount: u64,
    },
    InvalidMountInformation,
    CwdChanged,
}

/// Serializable failure diagnostics cannot reproduce the nonserializable live witness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ObjectQualificationFailure {
    #[error("held object qualification Unsupported: {0:?}")]
    Unsupported(ObjectQualificationUnsupported),
    #[error("held object qualification Unknown: {0:?}")]
    Unknown(ObjectQualificationUnknown),
}

type Result<T> = std::result::Result<T, ObjectQualificationFailure>;

fn unknown_io(stage: ObjectQualificationStage, error: io::Error) -> ObjectQualificationFailure {
    ObjectQualificationFailure::Unknown(ObjectQualificationUnknown::Io {
        stage,
        detail: error.to_string(),
    })
}

/// A passive retained current namespace. It is never transferred as an arbitrary join target.
pub(super) struct NamespaceContext {
    file: File,
}

impl NamespaceContext {
    pub fn capture() -> io::Result<Self> {
        Ok(Self {
            file: namespace_file()?,
        })
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }
    pub fn inode(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.ino())
    }
    pub fn matches_current(&self) -> io::Result<bool> {
        let expected = self.file.metadata()?;
        let actual = namespace_file()?.metadata()?;
        Ok((expected.dev(), expected.ino()) == (actual.dev(), actual.ino()))
    }
}

pub(super) fn namespace_file() -> io::Result<File> {
    // thread-self reflects the actual caller even if another thread has a different namespace.
    File::open("/proc/thread-self/ns/mnt")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountRecord {
    device: (u32, u32),
    filesystem: String,
    // Includes mount root/options. A changed context must not keep an earlier witness.
    record: String,
}

struct MountTable(HashMap<u64, MountRecord>);
impl MountTable {
    fn read() -> Result<Self> {
        let text = std::fs::read_to_string("/proc/thread-self/mountinfo")
            .map_err(|e| unknown_io(ObjectQualificationStage::MountInformation, e))?;
        Self::parse(&text)
    }
    fn parse(text: &str) -> Result<Self> {
        let mut records = HashMap::new();
        let malformed = || {
            ObjectQualificationFailure::Unknown(ObjectQualificationUnknown::InvalidMountInformation)
        };
        for line in text.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 10 {
                return Err(malformed());
            }
            let separator = fields
                .iter()
                .position(|field| *field == "-")
                .ok_or_else(malformed)?;
            if separator < 6 || fields.len() <= separator + 3 {
                return Err(malformed());
            }
            let id = fields[0].parse::<u64>().map_err(|_| malformed())?;
            let (major, minor) = fields[2].split_once(':').ok_or_else(malformed)?;
            let record = MountRecord {
                device: (
                    major.parse().map_err(|_| malformed())?,
                    minor.parse().map_err(|_| malformed())?,
                ),
                filesystem: fields[separator + 1].to_owned(),
                record: line.to_owned(),
            };
            if records.insert(id, record).is_some() {
                return Err(malformed());
            }
        }
        Ok(Self(records))
    }
    fn get(&self, mount: u64) -> Result<&MountRecord> {
        self.0
            .get(&mount)
            .ok_or(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::MountInformationMissing { mount },
            ))
    }
}

/// Borrowed evidence over the actual original FDs. No serde or scalar/path constructor.
pub struct QualifiedHeldObjects<'lease> {
    held: &'lease HeldSourceObjects,
    facts: Vec<QualifiedObjectFact>,
    mounts: Vec<MountRecord>,
}

impl HeldSourceObjects {
    /// IO happens before the caller takes its short canonical-index lock. Ext4 is an intentional
    /// OS/driver contract; the shared EXT magic alone does not admit ext2/ext3 or layered drivers.
    pub fn qualify_local(&self) -> Result<QualifiedHeldObjects<'_>> {
        if !cfg!(target_os = "linux") {
            return Err(ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::Platform,
            ));
        }
        check_namespace(self)?;
        if self.files.is_empty() || self.files.len() != self.shape.objects.len() {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::RoleClosureChanged,
            ));
        }
        let table = MountTable::read()?;
        let mut facts = Vec::new();
        let mut mounts = Vec::new();
        for (file, expected) in self.files.iter().zip(&self.shape.objects) {
            let (actual, mount) = qualify_file(file, expected.role.clone(), &table)?;
            if actual.object != *expected {
                return Err(ObjectQualificationFailure::Unknown(
                    ObjectQualificationUnknown::ObjectChanged(expected.role.clone()),
                ));
            }
            facts.push(actual);
            mounts.push(mount);
        }
        check_namespace(self)?;
        let refreshed = MountTable::read()?;
        for (fact, expected) in facts.iter().zip(&mounts) {
            if refreshed.get(fact.object.mount)? != expected {
                return Err(ObjectQualificationFailure::Unknown(
                    ObjectQualificationUnknown::MountInformationChanged {
                        mount: fact.object.mount,
                    },
                ));
            }
        }
        Ok(QualifiedHeldObjects {
            held: self,
            facts,
            mounts,
        })
    }
}

impl<'lease> QualifiedHeldObjects<'lease> {
    pub fn facts(&self) -> &[QualifiedObjectFact] {
        &self.facts
    }

    pub fn revalidate(&self) -> Result<()> {
        let current = self.held.qualify_local()?;
        if current.facts != self.facts {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::RoleClosureChanged,
            ));
        }
        for ((old, new), fact) in self.mounts.iter().zip(&current.mounts).zip(&self.facts) {
            if old != new {
                return Err(ObjectQualificationFailure::Unknown(
                    ObjectQualificationUnknown::MountInformationChanged {
                        mount: fact.object.mount,
                    },
                ));
            }
        }
        Ok(())
    }

    pub fn into_identity(self) -> HeldObjectIdentity<'lease> {
        let root = self.facts[0].object.clone();
        let layout = self
            .facts
            .iter()
            .skip(1)
            .map(|entry| entry.object.clone())
            .collect();
        HeldObjectIdentity {
            root: HeldRootKey {
                namespace: self.held.shape.mount_namespace,
                root,
                lease: std::marker::PhantomData,
            },
            layout,
            _qualification: self,
        }
    }

    /// Resolves the actual cwd from the held root. This is not future path confinement.
    pub fn open_cwd(self, cwd: &crate::identity::WorkspaceCwd) -> Result<QualifiedCwd<'lease>> {
        let directory = self
            .held
            .open_cwd(cwd)
            .map_err(|e| unknown_io(ObjectQualificationStage::Cwd, e))?;
        let table = MountTable::read()?;
        let object = fact(&directory, ObjectRole::Root)
            .map_err(|e| unknown_io(ObjectQualificationStage::Cwd, e))?;
        if object.mount != self.facts[0].object.mount {
            return Err(ObjectQualificationFailure::Unsupported(
                ObjectQualificationUnsupported::CwdMountBoundary {
                    root: self.facts[0].object.mount,
                    cwd: object.mount,
                },
            ));
        }
        let (qualified, mount) = qualify_file(&directory, ObjectRole::Root, &table)?;
        self.revalidate()?;
        Ok(QualifiedCwd {
            qualification: self,
            directory,
            fact: qualified,
            mount,
        })
    }
}

/// The exact cwd File survives qualification through fork and is CLOEXEC at helper exec.
pub struct QualifiedCwd<'lease> {
    qualification: QualifiedHeldObjects<'lease>,
    directory: File,
    fact: QualifiedObjectFact,
    mount: MountRecord,
}
impl QualifiedCwd<'_> {
    pub fn directory(&self) -> &File {
        &self.directory
    }
    pub fn revalidate(&self) -> Result<()> {
        self.qualification.revalidate()?;
        let current = qualify_file(&self.directory, ObjectRole::Root, &MountTable::read()?)?;
        if current.0 != self.fact || current.1 != self.mount {
            return Err(ObjectQualificationFailure::Unknown(
                ObjectQualificationUnknown::CwdChanged,
            ));
        }
        check_namespace(self.qualification.held)
    }
}

fn check_namespace(held: &HeldSourceObjects) -> Result<()> {
    let matches = held
        .namespace
        .matches_current()
        .map_err(|e| unknown_io(ObjectQualificationStage::Namespace, e))?;
    let inode = held
        .namespace
        .inode()
        .map_err(|e| unknown_io(ObjectQualificationStage::Namespace, e))?;
    if !matches || inode != held.shape.mount_namespace {
        return Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::NamespaceChanged,
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn qualify_file(
    file: &File,
    role: ObjectRole,
    table: &MountTable,
) -> Result<(QualifiedObjectFact, MountRecord)> {
    use std::os::fd::AsRawFd;
    let metadata = file
        .metadata()
        .map_err(|e| unknown_io(ObjectQualificationStage::Object(role.clone()), e))?;
    let kind_matches = match role {
        ObjectRole::GitEntry | ObjectRole::CommonLink => metadata.is_file(),
        ObjectRole::Root | ObjectRole::GitDirectory | ObjectRole::CommonDirectory => {
            metadata.is_dir() && metadata.nlink() > 0
        }
    };
    if !kind_matches {
        return Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::ObjectChanged(role),
        ));
    }
    let object = fact(file, role.clone())
        .map_err(|e| unknown_io(ObjectQualificationStage::Object(role.clone()), e))?;
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the original descriptor is held and statfs is a valid writable output buffer.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut filesystem) } != 0 {
        return Err(unknown_io(
            ObjectQualificationStage::Filesystem(role),
            io::Error::last_os_error(),
        ));
    }
    let mount = table.get(object.mount)?;
    const EXT_SUPER_MAGIC: u64 = 0xef53; // Linux magic.h; shared family magic is insufficient alone.
    let magic = filesystem.f_type as u64;
    if magic != EXT_SUPER_MAGIC || mount.filesystem != "ext4" {
        return Err(ObjectQualificationFailure::Unsupported(
            ObjectQualificationUnsupported::Filesystem {
                role,
                magic,
                filesystem: mount.filesystem.clone(),
            },
        ));
    }
    if mount.device != (libc::major(metadata.dev()), libc::minor(metadata.dev())) {
        return Err(ObjectQualificationFailure::Unknown(
            ObjectQualificationUnknown::ObjectChanged(role),
        ));
    }
    Ok((
        QualifiedObjectFact {
            object,
            profile: LocalObjectProfile::Ext4DriverHeldV1,
        },
        mount.clone(),
    ))
}

#[cfg(not(target_os = "linux"))]
fn qualify_file(
    _: &File,
    _: ObjectRole,
    _: &MountTable,
) -> Result<(QualifiedObjectFact, MountRecord)> {
    Err(ObjectQualificationFailure::Unsupported(
        ObjectQualificationUnsupported::Platform,
    ))
}

#[cfg(test)]
mod tests;
