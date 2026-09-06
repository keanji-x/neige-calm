//! The only place in this module that touches a filesystem, and the reason it
//! exists is that three review rounds of fixing call sites did not work.
//!
//! # What kept going wrong
//!
//! The threat model has been constant since #1515: an agent with write access
//! to the workspace, and therefore to `<workspace>/.neige/attachments/`. Under
//! that model a *path* is not a name for a file, it is a name the kernel
//! resolves again on every syscall, and anything on it can have been replaced
//! since the last time it was looked at.
//!
//! Every round closed the path the previous round reported and shipped the
//! next one:
//!
//! | round | closed | shipped |
//! |---|---|---|
//! | #1515 r2/r3 | `lstat`-then-`open` on the read | `O_NOFOLLOW` only covering the final component |
//! | #1515 r4 | the read, via `openat2` | — |
//! | #1505 S6 | — | the bind's destination: `create_dir_all` + `rename` on a joined path |
//! | S6 review 1 | the bind's destination | the sweep's `read_dir`/`remove_file`, and the whole upload write path |
//!
//! Each fix was correct and each was an enumeration: *these* call sites now
//! resolve safely. The defect was never a call site. It was that
//! `std::fs::read_dir`, `File::create`, `fs::rename`, `create_dir_all` and
//! `remove_file` **on a joined path** were all expressible here, so the next
//! reader — or the next slice — could reach for one without doing anything
//! unusual, and the next review would find it.
//!
//! # What this module does instead
//!
//! It makes the unguarded operation unexpressable rather than merely absent.
//!
//! * A caller gets [`CardDirs`], which holds two open directory descriptors.
//!   There is no accessor that yields a `Path`, a `PathBuf` or a `RawFd`, so a
//!   caller cannot join anything onto them or hand them to `std::fs`.
//! * Every operation takes a descriptor plus a [`Name`] — a validated single
//!   component with no `/`, no `.`, no `..` and no interior NUL. The syscalls
//!   underneath (`openat` with `O_EXCL | O_NOFOLLOW`, `renameat`, `unlinkat`,
//!   `mkdirat`, `fstatat` with `AT_SYMLINK_NOFOLLOW`, `fsync`) resolve exactly
//!   that one component relative to that one descriptor. There is no
//!   intermediate component for anything to swap, and a descriptor cannot be
//!   re-pointed.
//! * Deletion takes a [`StagingFd`] and nothing else. [`BoundFd`] has no
//!   conversion into one; `tests/ui/bound_fd_cannot_be_deleted.rs` fails to
//!   compile the moment somebody adds one.
//!
//! [`open_card_dirs`] is the single function here that names a path at all,
//! and it performs no filesystem call of its own: it hands the path to
//! [`crate::routes::fs::open_workspace_directory`], which is the `openat2`
//! primitive this repository already audited, and gets descriptors back.
//!
//! # The audit is a grep, and it is a test
//!
//! `planner_attachments_guarded_surface` in `crates/calm-server/tests/` fails
//! if any file under `planner_attachments/` except this one names a
//! path-resolving filesystem function. That is a lexical prohibition, checked
//! over the whole module, and it stays true as the module grows — unlike "we
//! checked every call site", which has now been wrong three times.
//!
//! **What the grep cannot see**, said plainly rather than left to be
//! discovered: it matches names. Code that reached the same syscalls through
//! an alias, a re-export under another name, or raw `libc` would not be
//! matched. The banned list therefore includes the module prefixes
//! (`std::fs`, `tokio::fs`, `libc::`) and the `use` forms that would bring
//! them in unqualified, which is what closes the ordinary ways of writing it;
//! a deliberate rename is out of its reach and is not something it claims to
//! stop.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, SystemTime};

use calm_types::planner_attachment::AttachmentId;

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::routes::fs::{
    WorkspaceSymlinks, open_workspace_directory, open_workspace_root_directory,
};

/// One path component, validated.
///
/// The point of the type is that it cannot describe a traversal: a value of it
/// is always a single name that `openat`-family syscalls resolve relative to a
/// descriptor. `AttachmentId` is already a stricter grammar than this, so
/// [`Name::of`] cannot fail; [`Name::parse`] exists for names read back off a
/// directory, which nothing in this module minted and which are therefore
/// checked.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(String);

impl Name {
    /// An attachment's own name.
    pub fn of(id: &AttachmentId) -> Self {
        Name(id.as_str().to_string())
    }

    /// A BIND's temporary, unique to one attempt.
    ///
    /// Unique per attempt rather than per attachment, and that is
    /// load-bearing HERE and not in the upload, because the two differ in
    /// whether the id is fresh.
    ///
    /// A bind is handed an id it did not mint, and two binds on the same id
    /// are ordinary — the browser retries a failed send, and the retry carries
    /// the same attachment ids. With a fixed `<id>.part` those two attempts
    /// collided, and the loser resolved the collision by unlinking the
    /// winner's temporary and renaming its own over the top: a partially
    /// written file published under a name in the one directory nothing may
    /// delete from. A fresh name per attempt means two attempts never name the
    /// same file, so there is no collision to resolve.
    pub fn temporary() -> Self {
        Name(format!("{}.part", uuid::Uuid::new_v4()))
    }

    /// An UPLOAD's temporary, derived from the id it will become.
    ///
    /// Derived rather than random, and safe to derive, because an upload mints
    /// its own id: the id is already unique to this request, so the temporary
    /// is too. Keeping it derivable is what lets a test that has seen the
    /// temporary know the name the publish will use, which is how the
    /// publish-failure path is exercised at all.
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

    /// Whether this is one of [`Name::temporary`]'s.
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

/// One card's two directories.
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

/// Read-only operations are the same for either directory; the write and
/// delete ones are not, and name the concrete type instead.
///
/// **Sealed.** The supertrait is private, so the only implementors are the two
/// in this file. That is what stops a later slice adding a third directory
/// type that the read side would accept — and, more to the point, keeps the
/// set of things a descriptor can be closed over.
pub trait DirFd: sealed::Sealed {
    #[doc(hidden)]
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

/// One entry of a directory listing: a regular file, its size and its mtime.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Name,
    pub len: u64,
    pub modified: SystemTime,
}

/// Resolve a card's two directories, creating `bound/` if it is not there yet.
///
/// The one function here that names a path, and it makes no filesystem call
/// itself — [`open_workspace_directory`] does, with `openat2` under
/// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`. `staging/` is created by the
/// upload before this is ever reached, so only `bound/` is created here.
///
/// `mkdirat` against the card's own descriptor rather than `create_dir_all` on
/// a joined path: the latter walks and follows every component, so a replaced
/// `<card>` would have it create — and later write into — a directory
/// somewhere else. `EEXIST` is not an error, because two requests on one card
/// can race to create it.
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
///
/// The upload's entry point, and the one caller that can arrive before any of
/// it exists: `<workspace>/.neige`, `.neige/attachments`, `attachments/<card>`
/// and `<card>/staging` are all created here if missing.
///
/// Every level is `mkdirat` against the descriptor of the level above, walking
/// down from `<workspace>` — which is the trust base, taken from the track's
/// stored path — and reopening each level through the guarded opener. Never
/// `create_dir_all` on a joined path: that follows every component, so a
/// replaced `.neige` or `<card>` would have it create, and every later write
/// land, somewhere else entirely. `EEXIST` is not an error at any level,
/// because two uploads on one card race to create them.
pub async fn create_card_dirs(workspace: &Path, root: &Path, card_id: &CardId) -> Result<CardDirs> {
    // `<workspace>` -> `.neige`
    let workspace_fd = open_workspace_root_directory(workspace)
        .await
        .map_err(opaque("the track workspace"))?;
    mkdir_in(workspace_fd, super::NEIGE_DIR).await?;

    // `.neige` -> `attachments`
    let neige = open_workspace_directory(workspace, super::NEIGE_DIR, WorkspaceSymlinks::Refused)
        .await
        .map_err(opaque("the server-owned subtree"))?;
    mkdir_in(neige, ATTACHMENTS).await?;

    // `attachments` -> `<card>`
    let root_fd = open_workspace_root_directory(root)
        .await
        .map_err(opaque("the attachment root"))?;
    mkdir_in(root_fd, card_id.as_str()).await?;

    // `<card>` -> `staging`
    let card = open_workspace_directory(root, card_id.as_str(), WorkspaceSymlinks::Refused)
        .await
        .map_err(opaque("this card's directory"))?;
    mkdir_in(card, STAGING).await?;

    open_card_dirs(root, card_id).await
}

const ATTACHMENTS: &str = "attachments";
const STAGING: &str = "staging";

/// `mkdirat` one name relative to an owned descriptor, tolerating `EEXIST`.
///
/// The descriptor is MOVED in and dropped inside the closure. A `RawFd` copied
/// into a `spawn_blocking` while its owner stayed in the async frame was a
/// real defect here: a client disconnecting drops the future, `close(fd)`
/// runs, another thread is handed the same number, and the detached closure
/// then operates on whatever now holds it.
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

/// The opener's errors name host paths. They go to the log; the client is told
/// which directory, and nothing about where it is.
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

/// `O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW`, relative to `staging/`.
///
/// `O_EXCL` with `O_CREAT` refuses an existing name and refuses to follow a
/// symlink sitting on it, and there is no other component to follow.
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

/// `renameat` within `staging/` — the upload's publish under its final name.
pub fn rename_within_staging(staging: &StagingFd, from: &Name, to: &Name) -> std::io::Result<()> {
    rename(staging.borrow(), from, staging.borrow(), to)
}

/// `renameat` from `staging/` into `bound/` — the bind's publish.
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

/// `unlinkat` in `staging/`. The one deletion door, and it takes a
/// [`StagingFd`].
///
/// A missing name is success: every caller has already committed something
/// that makes this file redundant.
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

/// `fsync` on a directory, which is what makes a `renameat` durable. The file
/// contents' own `sync_all` says nothing about the entry.
pub fn sync_staging(staging: &StagingFd) -> std::io::Result<()> {
    nix::unistd::fsync(staging.0.as_raw_fd()).map_err(std::io::Error::from)
}

pub fn sync_bound(bound: &BoundFd) -> std::io::Result<()> {
    nix::unistd::fsync(bound.0.as_raw_fd()).map_err(std::io::Error::from)
}

/// Every REGULAR FILE directly in the directory, with its size and mtime.
///
/// Anything that is not a regular file — a symlink, a socket, a subdirectory —
/// is stepped over rather than refused. Something with write access to this
/// workspace can plant one, and making a single planted entry abort the
/// enumeration would turn both readers of this function into a latch: the
/// budget would refuse every later upload on that card forever, and the sweep
/// would stop reclaiming. The stat is `fstatat` with `AT_SYMLINK_NOFOLLOW`, so
/// a link is described rather than followed.
///
/// A read failure that is NOT "this entry vanished" aborts with `Err` and the
/// caller gets nothing: a filesystem that will not answer means the sizes and
/// ages here are unknown, and both callers must treat an unknown as a reason
/// to keep bytes rather than to spend or delete them.
pub fn regular_entries<D: DirFd>(dir: &D) -> std::io::Result<Vec<Entry>> {
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;

    let handle = dir.borrow().try_clone_to_owned()?;
    let mut listing = nix::dir::Dir::from(handle).map_err(std::io::Error::from)?;
    let mut entries = Vec::new();
    let dirfd = dir.borrow().as_raw_fd();
    for entry in listing.iter() {
        let entry = entry.map_err(std::io::Error::from)?;
        let raw = entry.file_name().to_str().map_err(|_| {
            // A non-UTF-8 name cannot have been minted here. It is not an
            // error, but it is also not one of ours; treated below.
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
