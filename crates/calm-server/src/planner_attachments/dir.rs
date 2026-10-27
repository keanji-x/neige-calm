//! The only file under `planner_attachments/` that makes filesystem calls: every operation takes an open directory descriptor plus a validated single-component [`Name`], so no path can be joined and re-resolved.
//! The `planner_attachments_guarded_surface` test fails if any other file in the module names a path-resolving filesystem function.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, SystemTime};

use calm_types::planner_attachment::AttachmentId;

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::routes::fs::{
    WorkspaceSymlinks, open_workspace_directory, open_workspace_root_directory,
};

/// One path component, validated: it cannot describe a traversal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(String);

impl Name {
    pub fn of(id: &AttachmentId) -> Self {
        Name(id.as_str().to_string())
    }

    /// A bind's temporary, unique to one attempt: two binds on the same id are ordinary (a retried send carries the same ids), and a fixed `<id>.part` let the loser rename a partial file over the winner's.
    pub fn temporary() -> Self {
        Name(format!("{}.part", uuid::Uuid::new_v4()))
    }

    /// An upload's temporary, derived from the id it will become; the id is already unique to the request, and a derivable name lets tests exercise the publish-failure path.
    pub fn part_of(id: &AttachmentId) -> Self {
        Name(format!("{}.part", id.as_str()))
    }

    /// Validate a name that came from somewhere else — a directory listing.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty() || raw == "." || raw == ".." || raw.contains('/') || raw.contains('\0') {
            return None;
        }
        Some(Name(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_temporary(&self) -> bool {
        self.0.ends_with(".part")
    }
}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// `<root>/<card>/staging` — uploaded, not yet named by any queue entry, and
/// the only directory anything is ever deleted from.
#[derive(Debug)]
pub struct StagingFd(OwnedFd);

/// `<root>/<card>/bound` — named by a queue entry at least once. Nothing
/// deletes from here, and there is deliberately no way to ask this type to.
#[derive(Debug)]
pub struct BoundFd(OwnedFd);

/// Compile-time fence: `StagingFd: From<BoundFd>` would let a bound directory reach `unlink_staged`. If that impl
/// exists, both blanket impls below apply, the `_` cannot be inferred, and the crate fails to build.
const _: fn() = || {
    trait AmbiguousIfFromBound<A> {
        fn check() {}
    }
    impl<T> AmbiguousIfFromBound<()> for T {}
    struct FromBound;
    impl<T: From<BoundFd>> AmbiguousIfFromBound<FromBound> for T {}
    let _ = <StagingFd as AmbiguousIfFromBound<_>>::check;
};

#[derive(Debug)]
pub struct CardDirs {
    staging: StagingFd,
    bound: BoundFd,
}

impl CardDirs {
    pub fn staging(&self) -> &StagingFd {
        &self.staging
    }

    pub fn bound(&self) -> &BoundFd {
        &self.bound
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::StagingFd {}
    impl Sealed for super::BoundFd {}
}

/// Read-only operations are the same for either directory; write and delete ones name the concrete type.
/// Sealed so no third directory type can be added; `pub(crate)` because `borrow().as_raw_fd()` is one call from a `RawFd` that `std::fs` will take.
pub(crate) trait DirFd: sealed::Sealed {
    fn borrow(&self) -> BorrowedFd<'_>;
}

impl DirFd for StagingFd {
    fn borrow(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DirFd for BoundFd {
    fn borrow(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Name,
    pub len: u64,
    pub modified: SystemTime,
}

/// Resolve a card's two directories, creating `bound/` if it is not there yet.
/// `mkdirat` against the card's own descriptor rather than `create_dir_all` on a joined path, which follows every component; `EEXIST` is not an error because two requests on one card can race.
pub async fn open_card_dirs(root: &Path, card_id: &CardId) -> Result<CardDirs> {
    let card = open_workspace_directory(root, card_id.as_str(), WorkspaceSymlinks::Refused)
        .await
        .map_err(opaque("this card's directory"))?;
    mkdir_in(card, BOUND).await?;

    let staging = open_workspace_directory(
        root,
        &format!("{}/{STAGING}", card_id.as_str()),
        WorkspaceSymlinks::Refused,
    )
    .await
    .map_err(opaque("this card's staging directory"))?;
    let bound = open_workspace_directory(
        root,
        &format!("{}/{BOUND}", card_id.as_str()),
        WorkspaceSymlinks::Refused,
    )
    .await
    .map_err(opaque("this card's bound directory"))?;
    Ok(CardDirs {
        staging: StagingFd(staging),
        bound: BoundFd(bound),
    })
}

const BOUND: &str = "bound";

/// Create the whole chain up to `staging/`, then resolve both directories.
/// Every level is `mkdirat` against the descriptor of the level above, never `create_dir_all` on a joined path; `EEXIST` is not an error at any level.
pub async fn create_card_dirs(workspace: &Path, root: &Path, card_id: &CardId) -> Result<CardDirs> {
    let workspace_fd = open_workspace_root_directory(workspace)
        .await
        .map_err(opaque("the track workspace"))?;
    mkdir_in(workspace_fd, super::NEIGE_DIR).await?;

    let neige = open_workspace_directory(workspace, super::NEIGE_DIR, WorkspaceSymlinks::Refused)
        .await
        .map_err(opaque("the server-owned subtree"))?;
    mkdir_in(neige, ATTACHMENTS).await?;

    let root_fd = open_workspace_root_directory(root)
        .await
        .map_err(opaque("the attachment root"))?;
    mkdir_in(root_fd, card_id.as_str()).await?;

    let card = open_workspace_directory(root, card_id.as_str(), WorkspaceSymlinks::Refused)
        .await
        .map_err(opaque("this card's directory"))?;
    mkdir_in(card, STAGING).await?;

    open_card_dirs(root, card_id).await
}

const ATTACHMENTS: &str = "attachments";
const STAGING: &str = "staging";

/// `mkdirat` one name relative to an owned descriptor, tolerating `EEXIST`.
/// The descriptor is MOVED in and dropped inside the closure: a `RawFd` copied into `spawn_blocking` while its owner stayed in the async frame gets closed on client disconnect and reused by another thread.
async fn mkdir_in(parent: std::os::fd::OwnedFd, name: &str) -> Result<()> {
    use nix::sys::stat::{Mode, mkdirat};
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        match mkdirat(
            Some(parent.as_raw_fd()),
            name.as_str(),
            Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => Ok(()),
            Err(error) => Err(error),
        }
        // `parent` is dropped here, after the syscall, never before it.
    })
    .await
    .map_err(join_failed)?
    .map_err(|error: nix::errno::Errno| {
        tracing::error!(
            target: "planner_attachments::dir",
            %error,
            "could not create an attachment directory"
        );
        CalmError::Internal(
            "planner attachments: an attachment directory could not be created".into(),
        )
    })
}

/// The opener's errors name host paths: they go to the log, the client is told only which directory.
fn opaque(what: &'static str) -> impl Fn(CalmError) -> CalmError {
    move |error: CalmError| {
        tracing::error!(
            target: "planner_attachments::dir",
            directory = what,
            %error,
            "an attachment directory did not resolve"
        );
        CalmError::Internal(format!(
            "planner attachments: {what} is not usable for attachments"
        ))
    }
}

fn join_failed(error: tokio::task::JoinError) -> CalmError {
    CalmError::Internal(format!(
        "planner attachments: a filesystem step did not complete: {error}"
    ))
}

/// `O_EXCL` with `O_CREAT` refuses an existing name and refuses to follow a symlink sitting on it; there is no other component to follow.
pub fn create_new(staging: &StagingFd, name: &Name) -> std::io::Result<std::fs::File> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    use std::os::fd::FromRawFd;

    let raw = openat(
        Some(staging.0.as_raw_fd()),
        name.as_str(),
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(std::io::Error::from)?;
    // SAFETY: `openat` returned a new owned descriptor and this is its only
    // conversion into an owning Rust value.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

pub fn rename_within_staging(staging: &StagingFd, from: &Name, to: &Name) -> std::io::Result<()> {
    rename(staging.borrow(), from, staging.borrow(), to)
}

pub fn rename_into_bound(
    staging: &StagingFd,
    from: &Name,
    bound: &BoundFd,
    to: &Name,
) -> std::io::Result<()> {
    rename(staging.borrow(), from, bound.borrow(), to)
}

fn rename(
    from_dir: BorrowedFd<'_>,
    from: &Name,
    to_dir: BorrowedFd<'_>,
    to: &Name,
) -> std::io::Result<()> {
    nix::fcntl::renameat(
        Some(from_dir.as_raw_fd()),
        from.as_str(),
        Some(to_dir.as_raw_fd()),
        to.as_str(),
    )
    .map_err(std::io::Error::from)
}

/// The one deletion door, and it takes a [`StagingFd`]. A missing name is success: every caller has already committed something that makes this file redundant.
pub fn unlink_staged(staging: &StagingFd, name: &Name) -> std::io::Result<()> {
    use nix::unistd::{UnlinkatFlags, unlinkat};
    match unlinkat(
        Some(staging.0.as_raw_fd()),
        name.as_str(),
        UnlinkatFlags::NoRemoveDir,
    ) {
        Ok(()) | Err(nix::errno::Errno::ENOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error)),
    }
}

/// `fsync` on a directory is what makes a `renameat` durable; the file's own `sync_all` says nothing about the entry.
pub fn sync_staging(staging: &StagingFd) -> std::io::Result<()> {
    nix::unistd::fsync(staging.0.as_raw_fd()).map_err(std::io::Error::from)
}

pub fn sync_bound(bound: &BoundFd) -> std::io::Result<()> {
    nix::unistd::fsync(bound.0.as_raw_fd()).map_err(std::io::Error::from)
}

/// Every regular file directly in the directory, with its size and mtime.
/// Non-regular entries are stepped over rather than refused, so a planted entry cannot latch the budget or the sweep; a read failure other than ENOENT aborts, and callers must treat an unknown as a reason to keep bytes.
pub(crate) fn regular_entries<D: DirFd>(dir: &D) -> std::io::Result<Vec<Entry>> {
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;

    let handle = dir.borrow().try_clone_to_owned()?;
    let mut listing = nix::dir::Dir::from(handle).map_err(std::io::Error::from)?;
    let mut entries = Vec::new();
    let dirfd = dir.borrow().as_raw_fd();
    for entry in listing.iter() {
        let entry = entry.map_err(std::io::Error::from)?;
        let raw = entry.file_name().to_str().map_err(|_| {
            // A non-UTF-8 name cannot have been minted here; skipped.
            std::io::Error::new(std::io::ErrorKind::InvalidData, "non-utf8 name")
        });
        let Ok(raw) = raw else { continue };
        let Some(name) = Name::parse(raw) else {
            // `.` and `..`.
            continue;
        };
        let stat = match fstatat(Some(dirfd), name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(nix::errno::Errno::ENOENT) => continue,
            Err(error) => return Err(std::io::Error::from(error)),
        };
        if stat.st_mode & libc_s_ifmt() != libc_s_ifreg() {
            continue;
        }
        let modified = SystemTime::UNIX_EPOCH
            + Duration::new(
                stat.st_mtime.max(0) as u64,
                stat.st_mtime_nsec.max(0) as u32,
            );
        entries.push(Entry {
            name,
            len: stat.st_size.max(0) as u64,
            modified,
        });
    }
    Ok(entries)
}

fn libc_s_ifmt() -> nix::sys::stat::mode_t {
    nix::libc::S_IFMT
}

fn libc_s_ifreg() -> nix::sys::stat::mode_t {
    nix::libc::S_IFREG
}

#[cfg(target_os = "linux")]
pub(super) fn inventory_raw(
    root: &crate::routes::fs::TrustedReadRoot,
    bytes: &mut impl FnMut(&Path, &mut dyn std::io::Read) -> std::io::Result<()>,
) -> std::result::Result<Vec<super::export_read::RawEntry>, super::export_read::PartialRead> {
    use super::export_read::{Diagnostic, EntryKind, Identity, PartialRead, RawEntry};
    use crate::routes::fs::TrustedReadRoot;
    use sha2::{Digest, Sha256};
    use std::io::{self, Read};
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    use std::path::PathBuf;

    fn identity(meta: std::fs::Metadata) -> Identity {
        Identity {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode(),
            size: meta.size(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    fn listing(root: &TrustedReadRoot) -> io::Result<Vec<(PathBuf, Identity)>> {
        use nix::fcntl::AtFlags;
        use nix::sys::stat::fstatat;
        // A new open description is required: dup would share the directory offset.
        let handle = root.directory(Path::new("."))?;
        let mut dir = nix::dir::Dir::from(OwnedFd::from(handle.0)).map_err(io::Error::from)?;
        let mut result = Vec::new();
        for entry in dir.iter() {
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(name);
            let stat = fstatat(Some(root.0.as_raw_fd()), name, AtFlags::AT_SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            result.push((
                PathBuf::from(name),
                Identity {
                    device: stat.st_dev,
                    inode: stat.st_ino,
                    mode: stat.st_mode,
                    size: stat.st_size as u64,
                    modified: (stat.st_mtime, stat.st_mtime_nsec),
                    changed: (stat.st_ctime, stat.st_ctime_nsec),
                },
            ));
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(result)
    }
    fn read_source(file: &mut std::fs::File, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(test)]
        read_fault::before_read()?;
        file.read(buffer)
    }
    struct CheckedRead<'a> {
        file: &'a mut std::fs::File,
        hash: Sha256,
        count: u64,
        first_error: Option<io::Error>,
    }
    impl Read for CheckedRead<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let len = match read_source(self.file, buffer) {
                Ok(len) => len,
                Err(error) => {
                    // Retain the original cause for PartialRead, even if the callback retries.
                    let reported = match error.raw_os_error() {
                        Some(errno) => io::Error::from_raw_os_error(errno),
                        None => io::Error::new(error.kind(), error.to_string()),
                    };
                    if self.first_error.is_none() {
                        self.first_error = Some(error);
                    }
                    return Err(reported);
                }
            };
            self.count += len as u64;
            self.hash.update(&buffer[..len]);
            Ok(len)
        }
    }
    fn changed() -> io::Error {
        io::Error::other("frozen source changed")
    }
    fn walk(
        root: &TrustedReadRoot,
        prefix: &Path,
        result: &mut Vec<RawEntry>,
        bytes: &mut impl FnMut(&Path, &mut dyn Read) -> io::Result<()>,
    ) -> std::result::Result<(), PartialRead> {
        let fail = |error| PartialRead {
            relative: prefix.to_path_buf(),
            error,
        };
        let before = identity(root.0.metadata().map_err(fail)?);
        let entries = listing(root).map_err(fail)?;
        for (name, expected) in &entries {
            let path = prefix.join(name);
            let fail = |error| PartialRead {
                relative: path.clone(),
                error,
            };
            let kind = match expected.mode & nix::libc::S_IFMT {
                nix::libc::S_IFDIR => EntryKind::Directory,
                nix::libc::S_IFREG => EntryKind::Regular,
                nix::libc::S_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            let mut diagnostics = vec![Diagnostic::UninterpretedOwnership];
            if name.to_str().is_none() {
                diagnostics.push(Diagnostic::NonUtf8Name);
            }
            let mut sha256 = None;
            match kind {
                EntryKind::Directory => {
                    let child = root.directory(name).map_err(fail)?;
                    if identity(child.0.metadata().map_err(fail)?) != *expected {
                        return Err(fail(changed()));
                    }
                    walk(&child, &path, result, bytes)?;
                }
                EntryKind::Regular => {
                    let mut file = root.regular(name).map_err(fail)?;
                    if identity(file.metadata().map_err(fail)?) != *expected {
                        return Err(fail(changed()));
                    }
                    let mut input = CheckedRead {
                        file: &mut file,
                        hash: Sha256::new(),
                        count: 0,
                        first_error: None,
                    };
                    let callback = bytes(&path, &mut input);
                    if let Some(error) = input.first_error.take() {
                        return Err(fail(error));
                    }
                    callback.map_err(fail)?;
                    let eof = input.read(&mut [0u8; 1]);
                    if let Some(error) = input.first_error.take() {
                        return Err(fail(error));
                    }
                    if eof.map_err(fail)? != 0 || input.count != expected.size {
                        return Err(fail(changed()));
                    }
                    sha256 = Some(input.hash.finalize().into());
                    if identity(file.metadata().map_err(fail)?) != *expected {
                        return Err(fail(changed()));
                    }
                }
                _ => diagnostics.push(Diagnostic::TargetNotRead),
            }
            result.push(RawEntry {
                relative: path,
                kind,
                identity: expected.clone(),
                sha256,
                diagnostics,
            });
        }
        if listing(root).map_err(fail)? != entries
            || identity(root.0.metadata().map_err(fail)?) != before
        {
            return Err(fail(changed()));
        }
        Ok(())
    }
    let mut result = Vec::new();
    walk(root, Path::new(""), &mut result, bytes)?;
    Ok(result)
}

// Only the offline reader's underlying FD read is faultable; production has no switch.
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod read_fault {
    use std::{cell::RefCell, collections::VecDeque, io};

    thread_local! {
        static FAULTS: RefCell<Option<VecDeque<Option<i32>>>> = const { RefCell::new(None) };
    }

    pub(crate) struct Scope;

    pub(crate) fn install(reads: impl IntoIterator<Item = Option<i32>>) -> Scope {
        FAULTS.with(|faults| {
            let mut faults = faults.borrow_mut();
            assert!(faults.is_none(), "nested read fault scope");
            *faults = Some(reads.into_iter().collect());
        });
        Scope
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            FAULTS.with(|faults| *faults.borrow_mut() = None);
        }
    }

    pub(super) fn before_read() -> io::Result<()> {
        let fault = FAULTS.with(|faults| {
            faults
                .borrow_mut()
                .as_mut()
                .and_then(VecDeque::pop_front)
                .flatten()
        });
        match fault {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }
}
