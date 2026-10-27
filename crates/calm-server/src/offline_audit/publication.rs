use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Component, Path, PathBuf};

use super::{Identity, TrustedReadRoot};
use nix::fcntl::{OFlag, RenameFlags, openat, renameat2};
use nix::sys::stat::{Mode, mkdirat};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    CreateDirectory,
    CreateFile,
    Read,
    Write,
    FileSync,
    DirectorySync(PathBuf),
    ValidateBindings,
    Rename,
    ParentSync,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PublishedDurable;

#[derive(Debug)]
pub enum PublicationError {
    InvalidDestination,
    Overlap,
    Unsupported,
    Partial {
        temporary: OsString,
        stage: Stage,
        error: io::Error,
    },
    PublishFailed {
        temporary: OsString,
        error: io::Error,
    },
    PublishedDurabilityUnconfirmed {
        destination: OsString,
        error: io::Error,
    },
    AlreadyFailed,
}

trait Io: Send {
    fn write(&mut self, file: &mut File, data: &[u8]) -> io::Result<()> {
        file.write_all(data)
    }
    fn sync(&mut self, file: &File, _stage: &Stage) -> io::Result<()> {
        file.sync_all()
    }
    fn rename(&mut self, parent: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        renameat2(
            Some(parent.as_raw_fd()),
            from,
            Some(parent.as_raw_fd()),
            to,
            RenameFlags::RENAME_NOREPLACE,
        )
        .map_err(io::Error::from)
    }
}
struct SystemIo;
impl Io for SystemIo {}

struct Directory {
    root: TrustedReadRoot,
    mode: u32,
}
impl Directory {
    fn new(root: TrustedReadRoot) -> io::Result<Self> {
        let mode = root.0.metadata()?.mode();
        Ok(Self { root, mode })
    }
    fn matches(&self, actual: &TrustedReadRoot) -> io::Result<bool> {
        Ok(identity(&self.root.0)? == identity(&actual.0)?
            && actual.0.metadata()?.mode() == self.mode)
    }
}

fn snapshot(file: &File) -> io::Result<Identity> {
    let meta = file.metadata()?;
    Ok(Identity {
        device: meta.dev(),
        inode: meta.ino(),
        mode: meta.mode(),
        size: meta.size(),
        modified: (meta.mtime(), meta.mtime_nsec()),
        changed: (meta.ctime(), meta.ctime_nsec()),
    })
}
fn changed() -> io::Error {
    io::Error::other("output binding or metadata changed")
}

/// An exclusive incomplete directory in the destination's already-open parent.
/// Dropping or failing preserves it; retries always create a different directory.
/// A successful `build` means PublishedDurable, not semantic audit eligibility.
pub struct Publisher {
    parent: TrustedReadRoot,
    destination: OsString,
    temporary: OsString,
    directories: BTreeMap<PathBuf, Directory>,
    files: BTreeMap<PathBuf, (File, Identity)>,
    sources: Vec<TrustedReadRoot>,
    io: Box<dyn Io>,
    failed: bool,
}

fn component(name: &Path) -> io::Result<&OsStr> {
    let mut parts = name.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) => Ok(name),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino()))
}
fn ancestor(ancestor: &File, child: &File) -> io::Result<bool> {
    let expected = identity(ancestor)?;
    let mut current = child.try_clone()?;
    loop {
        let here = identity(&current)?;
        if here == expected {
            return Ok(true);
        }
        let raw = openat(
            Some(current.as_raw_fd()),
            "..",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        // SAFETY: openat transferred a fresh descriptor to this sole owner.
        let parent = unsafe { File::from_raw_fd(raw) };
        if identity(&parent)? == here {
            return Ok(false);
        }
        current = parent;
    }
}

impl Publisher {
    /// The explicit destination must be fresh. `sources` are all independently
    /// frozen roots read by this export, never discovered by scanning host paths.
    /// Parent and sources remain FD authorities even if their pathnames change.
    /// The caller must exclusively control the output parent and tree throughout
    /// the build and publication. Binding/metadata observations do not freeze them
    /// or prevent a same-UID process from changing them after the last check.
    pub fn new(
        parent: TrustedReadRoot,
        destination: &OsStr,
        sources: &[&TrustedReadRoot],
    ) -> Result<Self, PublicationError> {
        component(Path::new(destination)).map_err(|_| PublicationError::InvalidDestination)?;
        let temporary: OsString = format!(".incomplete-{}", uuid::Uuid::new_v4()).into();
        let map = |error| PublicationError::Partial {
            temporary: temporary.clone(),
            stage: Stage::CreateDirectory,
            error,
        };
        for source in sources {
            if ancestor(&source.0, &parent.0).map_err(map)? {
                return Err(PublicationError::Overlap);
            }
        }
        let sources = sources
            .iter()
            .map(|source| source.directory(Path::new(".")))
            .collect::<io::Result<Vec<_>>>()
            .map_err(map)?;
        // Existing final targets are refused before any writes. This also refuses
        // the reverse overlap: a final target which is an ancestor of a source.
        match nix::sys::stat::fstatat(
            Some(parent.0.as_raw_fd()),
            destination,
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => return Err(PublicationError::InvalidDestination),
            Err(nix::errno::Errno::ENOENT) => (),
            Err(error) => return Err(map(error.into())),
        }
        mkdirat(
            Some(parent.0.as_raw_fd()),
            temporary.as_os_str(),
            Mode::from_bits_truncate(0o700),
        )
        .map_err(|e| map(e.into()))?;
        let root =
            Directory::new(parent.directory(Path::new(&temporary)).map_err(map)?).map_err(map)?;
        Ok(Self {
            parent,
            destination: destination.into(),
            temporary,
            directories: BTreeMap::from([(PathBuf::new(), root)]),
            files: BTreeMap::new(),
            sources,
            io: Box::new(SystemIo),
            failed: false,
        })
    }

    fn partial(&mut self, stage: Stage, error: io::Error) -> PublicationError {
        self.failed = true;
        PublicationError::Partial {
            temporary: self.temporary.clone(),
            stage,
            error,
        }
    }

    fn parent_of<'a>(&self, path: &'a Path) -> io::Result<(&TrustedReadRoot, &'a OsStr)> {
        if path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let name = component(Path::new(
            path.file_name().ok_or(io::ErrorKind::InvalidInput)?,
        ))?;
        let parent = self
            .directories
            .get(path.parent().unwrap_or(Path::new("")))
            .ok_or(io::ErrorKind::NotFound)?;
        Ok((&parent.root, name))
    }

    /// Add exactly one new directory below a previously declared directory.
    fn add_directory(&mut self, path: &Path) -> Result<(), PublicationError> {
        if self.failed {
            return Err(PublicationError::AlreadyFailed);
        }
        let operation = || -> io::Result<TrustedReadRoot> {
            let (parent, name) = self.parent_of(path)?;
            mkdirat(
                Some(parent.0.as_raw_fd()),
                name,
                Mode::from_bits_truncate(0o700),
            )
            .map_err(io::Error::from)?;
            parent.directory(Path::new(name))
        };
        let directory = operation().map_err(|e| self.partial(Stage::CreateDirectory, e))?;
        let directory =
            Directory::new(directory).map_err(|e| self.partial(Stage::CreateDirectory, e))?;
        self.directories.insert(path.into(), directory);
        Ok(())
    }

    /// Create a 0600 file once, stream all caller bytes, then sync it. This same
    /// door handles raw evidence, caller manifests and seals without interpreting them.
    fn write_file(
        &mut self,
        path: &Path,
        input: &mut (impl Read + ?Sized),
    ) -> Result<(), PublicationError> {
        if self.failed {
            return Err(PublicationError::AlreadyFailed);
        }
        let create = || -> io::Result<File> {
            let (parent, name) = self.parent_of(path)?;
            let raw = openat(
                Some(parent.0.as_raw_fd()),
                name,
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(io::Error::from)?;
            // SAFETY: openat returned a new descriptor transferred to one owner.
            Ok(unsafe { File::from_raw_fd(raw) })
        };
        let mut file = create().map_err(|e| self.partial(Stage::CreateFile, e))?;
        let mut buffer = [0u8; 65536];
        loop {
            let len = input
                .read(&mut buffer)
                .map_err(|e| self.partial(Stage::Read, e))?;
            if len == 0 {
                break;
            }
            self.io
                .write(&mut file, &buffer[..len])
                .map_err(|e| self.partial(Stage::Write, e))?;
        }
        let expected = snapshot(&file).map_err(|e| self.partial(Stage::FileSync, e))?;
        self.io
            .sync(&file, &Stage::FileSync)
            .map_err(|e| self.partial(Stage::FileSync, e))?;
        if snapshot(&file).map_err(|e| self.partial(Stage::FileSync, e))? != expected {
            return Err(self.partial(Stage::ValidateBindings, changed()));
        }
        self.files.insert(path.into(), (file, expected));
        Ok(())
    }

    fn watch_source(&mut self, source: &TrustedReadRoot) -> io::Result<()> {
        let root = &self.directories[Path::new("")].root;
        if ancestor(&source.0, &self.parent.0)? || ancestor(&root.0, &source.0)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source/output overlap",
            ));
        }
        let id = identity(&source.0)?;
        for watched in &self.sources {
            if identity(&watched.0)? == id {
                return Ok(());
            }
        }
        self.sources.push(source.directory(Path::new("."))?);
        Ok(())
    }

    fn validate_bindings(&self, root_name: &OsStr) -> io::Result<()> {
        let root = &self.directories[Path::new("")];
        if !root.matches(&self.parent.directory(Path::new(root_name))?)? {
            return Err(changed());
        }
        for source in &self.sources {
            if ancestor(&source.0, &self.parent.0)? || ancestor(&root.root.0, &source.0)? {
                return Err(changed());
            }
        }
        for (path, directory) in &self.directories {
            let before = snapshot(&directory.root.0)?;
            // Reopen rather than dup: a shared directory offset can silently omit names.
            let listing = directory.root.directory(Path::new("."))?;
            let mut listing =
                nix::dir::Dir::from(OwnedFd::from(listing.0)).map_err(io::Error::from)?;
            let mut count = 0;
            for entry in listing.iter() {
                let entry = entry.map_err(io::Error::from)?;
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                let name = Path::new(OsStr::from_bytes(name));
                let relative = path.join(name);
                if let Some(expected) = self.directories.get(&relative) {
                    if !expected.matches(&directory.root.directory(name)?)? {
                        return Err(changed());
                    }
                } else if let Some((file, expected)) = self.files.get(&relative) {
                    if snapshot(&directory.root.regular(name)?)? != *expected
                        || snapshot(file)? != *expected
                    {
                        return Err(changed());
                    }
                } else {
                    return Err(changed());
                }
                count += 1;
            }
            let expected_count = self
                .directories
                .keys()
                .chain(self.files.keys())
                .filter(|child| child.parent() == Some(path.as_path()))
                .count();
            if count != expected_count || snapshot(&directory.root.0)? != before {
                return Err(changed());
            }
        }
        if !root.matches(&self.parent.directory(Path::new(root_name))?)? {
            return Err(changed());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<PublishedDurable, PublicationError> {
        if self.failed {
            return Err(PublicationError::AlreadyFailed);
        }
        for (path, directory) in self.directories.iter().rev() {
            let stage = Stage::DirectorySync(path.clone());
            self.io
                .sync(&directory.root.0, &stage)
                .map_err(|error| PublicationError::Partial {
                    temporary: self.temporary.clone(),
                    stage,
                    error,
                })?;
        }
        self.validate_bindings(&self.temporary)
            .map_err(|e| self.partial(Stage::ValidateBindings, e))?;
        self.io
            .rename(&self.parent.0, &self.temporary, &self.destination)
            .map_err(|error| {
                if matches!(
                    error.raw_os_error(),
                    Some(nix::libc::ENOSYS | nix::libc::EINVAL | nix::libc::EOPNOTSUPP)
                ) {
                    PublicationError::Unsupported
                } else {
                    PublicationError::PublishFailed {
                        temporary: self.temporary.clone(),
                        error,
                    }
                }
            })?;
        // NOREPLACE protects the destination, not the rename source's FD binding.
        self.validate_bindings(&self.destination)
            .and_then(|()| self.io.sync(&self.parent.0, &Stage::ParentSync))
            .and_then(|()| self.validate_bindings(&self.destination))
            .map_err(|error| PublicationError::PublishedDurabilityUnconfirmed {
                destination: self.destination,
                error,
            })
            .map(|()| PublishedDurable)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod regressions;

mod build;
pub use build::{BuildError, BuildWriter};
#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod build_tests;
