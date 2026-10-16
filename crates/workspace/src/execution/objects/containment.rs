//! Positive current-directory evidence only. Failed lookup never proves disjointness.
use super::{HeldRootKey, HeldSourceObjects, ObjectRole, fact, namespace};
use std::{
    fs::File,
    io,
    marker::PhantomData,
    os::{
        fd::FromRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentKind {
    SameDirectory,
    StrictDescendant,
}

/// Both original object leases remain held. This is an observation, not future cwd confinement.
pub struct PositiveContainment<'ancestor, 'subject> {
    ancestor: HeldRootKey<'ancestor>,
    subject: HeldRootKey<'subject>,
    kind: ContainmentKind,
}
impl<'ancestor, 'subject> PositiveContainment<'ancestor, 'subject> {
    pub fn ancestor_key(&self) -> &HeldRootKey<'ancestor> {
        &self.ancestor
    }
    pub fn subject_key(&self) -> &HeldRootKey<'subject> {
        &self.subject
    }
    pub fn kind(&self) -> ContainmentKind {
        self.kind
    }
}

#[derive(Debug)]
pub enum ContainmentUnknown {
    ObjectMetadata(io::Error),
    DetachedDirectory,
    NamespaceChanged,
    NoQualifiedLocator,
    LocatorChanged,
    KernelLookup(io::Error),
}
#[derive(Debug)]
pub enum ContainmentUnsupported {
    Platform,
    DifferentMount,
    Kernel(io::Error),
}
pub enum DirectoryContainment<'ancestor, 'subject> {
    Verified(PositiveContainment<'ancestor, 'subject>),
    Unknown(ContainmentUnknown),
    Unsupported(ContainmentUnsupported),
}

impl HeldSourceObjects {
    /// Call outside the identity-index lock. A locator is only a hint: Linux must resolve beneath
    /// the actual held ancestor and return exactly the actual held subject. No negative is inferred.
    pub fn contains_directory<'ancestor, 'subject>(
        &'ancestor self,
        subject: &'subject HeldSourceObjects,
    ) -> DirectoryContainment<'ancestor, 'subject> {
        if !cfg!(target_os = "linux") {
            return DirectoryContainment::Unsupported(ContainmentUnsupported::Platform);
        }
        let current_namespace = match namespace() {
            Ok(namespace) => namespace,
            Err(error) => {
                return DirectoryContainment::Unknown(ContainmentUnknown::ObjectMetadata(error));
            }
        };
        if current_namespace != self.shape.mount_namespace
            || current_namespace != subject.shape.mount_namespace
        {
            return DirectoryContainment::Unknown(ContainmentUnknown::NamespaceChanged);
        }
        let ancestor_fact = match fact(self.root(), ObjectRole::Root) {
            Ok(fact) => fact,
            Err(error) => {
                return DirectoryContainment::Unknown(ContainmentUnknown::ObjectMetadata(error));
            }
        };
        let subject_fact = match fact(subject.root(), ObjectRole::Root) {
            Ok(fact) => fact,
            Err(error) => {
                return DirectoryContainment::Unknown(ContainmentUnknown::ObjectMetadata(error));
            }
        };
        for root in [self.root(), subject.root()] {
            match root.metadata() {
                Ok(metadata) if metadata.nlink() > 0 && metadata.is_dir() => {}
                Ok(_) => {
                    return DirectoryContainment::Unknown(ContainmentUnknown::DetachedDirectory);
                }
                Err(error) => {
                    return DirectoryContainment::Unknown(ContainmentUnknown::ObjectMetadata(
                        error,
                    ));
                }
            }
        }
        if ancestor_fact.mount != subject_fact.mount {
            return DirectoryContainment::Unsupported(ContainmentUnsupported::DifferentMount);
        }
        let kind = if ancestor_fact == subject_fact {
            ContainmentKind::SameDirectory
        } else {
            let relative = match subject.shape.root.strip_prefix(&self.shape.root) {
                Ok(relative) if !relative.as_os_str().is_empty() => relative,
                _ => return DirectoryContainment::Unknown(ContainmentUnknown::NoQualifiedLocator),
            };
            let opened = match open_beneath(self.root(), relative) {
                Ok(opened) => opened,
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::ENOSYS | libc::EINVAL | libc::E2BIG | libc::EPERM)
                    ) =>
                {
                    return DirectoryContainment::Unsupported(ContainmentUnsupported::Kernel(
                        error,
                    ));
                }
                Err(error) => {
                    return DirectoryContainment::Unknown(ContainmentUnknown::KernelLookup(error));
                }
            };
            match fact(&opened, ObjectRole::Root) {
                Ok(actual) if actual == subject_fact => ContainmentKind::StrictDescendant,
                Ok(_) => return DirectoryContainment::Unknown(ContainmentUnknown::LocatorChanged),
                Err(error) => {
                    return DirectoryContainment::Unknown(ContainmentUnknown::ObjectMetadata(
                        error,
                    ));
                }
            }
        };
        DirectoryContainment::Verified(PositiveContainment {
            ancestor: HeldRootKey {
                namespace: current_namespace,
                root: ancestor_fact,
                lease: PhantomData,
            },
            subject: HeldRootKey {
                namespace: current_namespace,
                root: subject_fact,
                lease: PhantomData,
            },
            kind,
        })
    }
}

#[cfg(target_os = "linux")]
fn open_beneath(root: &File, relative: &std::path::Path) -> io::Result<File> {
    use std::os::fd::AsRawFd;
    let name = std::ffi::CString::new(relative.as_os_str().as_bytes())?;
    // Linux openat2(2): BENEATH is kernel enforced; NO_XDEV excludes bind-mount aliases.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_XDEV;
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor as i32) })
    }
}
#[cfg(not(target_os = "linux"))]
fn open_beneath(_: &File, _: &std::path::Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory containment is not qualified on this platform",
    ))
}
