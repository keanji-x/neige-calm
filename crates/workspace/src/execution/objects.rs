//! Live object leases. Serialized facts are validation inputs, never reopened leases.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectRole {
    Root,
    GitDirectory,
    GitEntry,
    CommonDirectory,
    CommonLink,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectFact {
    pub role: ObjectRole,
    pub device: u64,
    pub inode: u64,
    pub mount: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkFact {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitEntry {
    Directory,
    File(LinkFact),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommonLayout {
    Same,
    Linked { path: PathBuf, link: LinkFact },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectLayout {
    ExternalDir,
    Worktree {
        git_directory: PathBuf,
        entry: GitEntry,
        common: CommonLayout,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectShape {
    pub root: PathBuf,
    pub mount_namespace: u64,
    pub layout: ObjectLayout,
    pub objects: Vec<ObjectFact>,
}

/// Ephemeral bucket over a currently retained root; never a serialized authority or Ready proof.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HeldRootKey<'lease> {
    namespace: u64,
    root: ObjectFact,
    lease: std::marker::PhantomData<&'lease HeldSourceObjects>,
}

pub struct HeldObjectIdentity<'lease> {
    root: HeldRootKey<'lease>,
    layout: Vec<ObjectFact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldObjectComparison {
    SameObject,
    DifferentRoot,
    LayoutConflict,
}

impl HeldObjectIdentity<'_> {
    pub fn root_key(&self) -> &HeldRootKey<'_> {
        &self.root
    }
    pub fn compare(&self, other: &HeldObjectIdentity<'_>) -> HeldObjectComparison {
        if self.root != other.root {
            HeldObjectComparison::DifferentRoot
        } else if self.layout == other.layout {
            HeldObjectComparison::SameObject
        } else {
            HeldObjectComparison::LayoutConflict
        }
    }
}

pub struct HeldSourceObjects {
    shape: ObjectShape,
    files: Vec<File>,
}
impl HeldSourceObjects {
    /// Trusted registration owner opens once, then transfers these exact held descriptions.
    pub fn open(root: &Path, worktree: bool) -> io::Result<Self> {
        let mut files = vec![open_directory(root)?];
        let mut roles = vec![ObjectRole::Root];
        let layout = if worktree {
            let git_directory = git_path(root, "--absolute-git-dir")?;
            files.push(open_directory(&git_directory)?);
            roles.push(ObjectRole::GitDirectory);
            let entry_path = root.join(".git");
            let entry = if std::fs::symlink_metadata(&entry_path)?.is_dir() {
                GitEntry::Directory
            } else {
                let (file, link) = open_link(&entry_path)?;
                files.push(file);
                roles.push(ObjectRole::GitEntry);
                GitEntry::File(link)
            };
            let common_path = git_path(root, "--git-common-dir")?;
            let common = if common_path == git_directory {
                CommonLayout::Same
            } else {
                files.push(open_directory(&common_path)?);
                roles.push(ObjectRole::CommonDirectory);
                let (file, link) = open_link(&git_directory.join("commondir"))?;
                files.push(file);
                roles.push(ObjectRole::CommonLink);
                CommonLayout::Linked {
                    path: common_path,
                    link,
                }
            };
            ObjectLayout::Worktree {
                git_directory,
                entry,
                common,
            }
        } else {
            ObjectLayout::ExternalDir
        };
        let objects = files
            .iter()
            .zip(roles)
            .map(|(file, role)| fact(file, role))
            .collect::<io::Result<_>>()?;
        let held = Self {
            shape: ObjectShape {
                root: root.into(),
                mount_namespace: namespace()?,
                layout,
                objects,
            },
            files,
        };
        held.validate()?;
        Ok(held)
    }
    /// Ownership transfers only from recvmsg. No path-open may fill a missing role.
    pub fn receive(shape: ObjectShape, descriptors: Vec<OwnedFd>) -> io::Result<Self> {
        let held = Self {
            shape,
            files: descriptors.into_iter().map(File::from).collect(),
        };
        held.validate()?;
        Ok(held)
    }
    /// Compute before the short identity-index lock. Comparison itself does no IO or Git.
    /// File descriptions stay continuously retained through the key's borrow lifetime.
    pub fn live_identity(&self) -> io::Result<HeldObjectIdentity<'_>> {
        let current_namespace = namespace()?;
        ensure(
            current_namespace == self.shape.mount_namespace,
            "live identity mount context changed",
        )?;
        ensure(
            self.files.len() == self.shape.objects.len(),
            "live identity descriptor closure missing",
        )?;
        let mut objects = self
            .files
            .iter()
            .zip(&self.shape.objects)
            .map(|(file, expected)| fact(file, expected.role.clone()))
            .collect::<io::Result<Vec<_>>>()?;
        ensure(!objects.is_empty(), "live identity root missing")?;
        let root = objects.remove(0);
        Ok(HeldObjectIdentity {
            root: HeldRootKey {
                namespace: current_namespace,
                root,
                lease: std::marker::PhantomData,
            },
            layout: objects,
        })
    }
    pub fn shape(&self) -> &ObjectShape {
        &self.shape
    }
    pub fn descriptors(&self) -> Vec<BorrowedFd<'_>> {
        self.files.iter().map(AsFd::as_fd).collect()
    }
    pub fn root(&self) -> &File {
        &self.files[0]
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            shape: self.shape.clone(),
            files: self
                .files
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        ensure(
            self.shape.root.is_absolute() && self.shape.mount_namespace == namespace()?,
            "unsupported object/mount context",
        )?;
        let entries = self.entries()?;
        ensure(
            entries.len() == self.files.len() && entries.len() == self.shape.objects.len(),
            "attachment descriptor count mismatch",
        )?;
        for ((file, expected), (role, path, content)) in
            self.files.iter().zip(&self.shape.objects).zip(entries)
        {
            ensure(
                expected.role == role && *expected == fact(file, role.clone())?,
                "attachment role/object identity mismatch",
            )?;
            let metadata = file.metadata()?;
            ensure(
                if content.is_some() {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                },
                "attachment object kind mismatch",
            )?;
            // A named object is compared to a continuously held descriptor, never substituted for it.
            let named_file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            let named = named_file.metadata()?;
            ensure(
                !named.file_type().is_symlink() && fact(&named_file, role.clone())? == *expected,
                "named object or mount replaced",
            )?;
            if let Some(bytes) = content {
                ensure(
                    read_link(file)? == bytes,
                    "Git indirection contents changed",
                )?;
            }
        }
        if let ObjectLayout::Worktree {
            git_directory,
            common,
            ..
        } = &self.shape.layout
        {
            ensure(
                git_path(&self.shape.root, "--show-toplevel")? == self.shape.root,
                "not a Git worktree root",
            )?;
            ensure(
                git_path(&self.shape.root, "--absolute-git-dir")? == *git_directory,
                "Git directory layout changed",
            )?;
            let common_path = match common {
                CommonLayout::Same => git_directory,
                CommonLayout::Linked { path, .. } => path,
            };
            ensure(
                git_path(&self.shape.root, "--git-common-dir")? == *common_path,
                "Git common-directory layout changed",
            )?;
        }
        Ok(())
    }
    fn entries(&self) -> io::Result<Vec<(ObjectRole, PathBuf, Option<Vec<u8>>)>> {
        let mut entries = vec![(ObjectRole::Root, self.shape.root.clone(), None)];
        if let ObjectLayout::Worktree {
            git_directory,
            entry,
            common,
        } = &self.shape.layout
        {
            ensure(git_directory.is_absolute(), "relative Git directory")?;
            entries.push((ObjectRole::GitDirectory, git_directory.clone(), None));
            match entry {
                GitEntry::Directory => ensure(
                    *git_directory == self.shape.root.join(".git"),
                    "inline Git entry mismatch",
                )?,
                GitEntry::File(link) => {
                    ensure(
                        link.path == self.shape.root.join(".git"),
                        "Git entry path mismatch",
                    )?;
                    entries.push((
                        ObjectRole::GitEntry,
                        link.path.clone(),
                        Some(link.bytes.clone()),
                    ));
                }
            }
            if let CommonLayout::Linked { path, link } = common {
                ensure(
                    path.is_absolute() && link.path == git_directory.join("commondir"),
                    "common link path mismatch",
                )?;
                entries.push((ObjectRole::CommonDirectory, path.clone(), None));
                entries.push((
                    ObjectRole::CommonLink,
                    link.path.clone(),
                    Some(link.bytes.clone()),
                ));
            }
        }
        Ok(entries)
    }
}
fn ensure(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}
fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn open_link(path: &Path) -> io::Result<(File, LinkFact)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let bytes = read_link(&file)?;
    Ok((
        file,
        LinkFact {
            path: path.into(),
            bytes,
        },
    ))
}
fn read_link(file: &File) -> io::Result<Vec<u8>> {
    ensure(
        file.metadata()?.is_file() && file.metadata()?.len() <= 65536,
        "invalid Git indirection file",
    )?;
    let mut bytes = vec![0; file.metadata()?.len() as usize];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}
fn fact(file: &File, role: ObjectRole) -> io::Result<ObjectFact> {
    use std::os::fd::AsRawFd;
    let stat = file.metadata()?;
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
    let mount = info
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .ok_or_else(|| io::Error::other("mount identity unavailable"))?
        .trim()
        .parse()
        .map_err(io::Error::other)?;
    Ok(ObjectFact {
        role,
        device: stat.dev(),
        inode: stat.ino(),
        mount,
    })
}
fn namespace() -> io::Result<u64> {
    Ok(std::fs::metadata("/proc/self/ns/mnt")?.ino())
}
fn git_path(root: &Path, flag: &str) -> io::Result<PathBuf> {
    let output = crate::command::neige_git_command()
        .arg("-C")
        .arg(root)
        .args(["rev-parse", flag])
        .output()?;
    ensure(output.status.success(), "Git layout inspection failed")?;
    let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    std::fs::canonicalize(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}
