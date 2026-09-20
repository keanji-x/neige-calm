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
