//! Filesystem-owner authority for scoped host reads. Canonical names select an
//! allowed root; only descriptors opened beneath that bound root supply bytes.
use super::*;
use crate::share_access::scope::ShareAccessScope;

pub(crate) struct ShareRead {
    #[cfg(target_os = "linux")]
    roots: Vec<(PathBuf, TrustedReadRoot)>,
}

fn denied() -> CalmError {
    CalmError::Forbidden("outside share scope".into())
}

impl ShareRead {
    pub(crate) async fn bind(
        scope: &ShareAccessScope,
        repo: &dyn crate::db::RouteRepo,
    ) -> Result<Self> {
        let paths = scope.roots(repo).await?;
        #[cfg(target_os = "linux")]
        {
            let mut roots = Vec::new();
            for path in paths {
                // Refuse symlinks in every component, including ancestors of the
                // canonical root. A swapped root must not grant new authority.
                let root = TrustedReadRoot::open_absolute(&path).map_err(|_| denied())?;
                roots.push((path, root));
            }
            Ok(Self { roots })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = paths;
            Err(denied())
        }
    }

    /// Canonical names select a capability, but never replace its secure open.
    /// Failed resolution must not disclose existence outside the allowed roots.
    pub(super) async fn resolve(&self, raw: &Path) -> Result<PathBuf> {
        let path = canonicalize_file_or_parent(raw)
            .await
            .map_err(|_| denied())?;
        if !self.contains(&path) {
            return Err(denied());
        }
        Ok(path)
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.roots.iter().any(|(root, _)| path.starts_with(root))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            false
        }
    }

    #[cfg(target_os = "linux")]
    fn root<'a>(&'a self, path: &'a Path) -> Result<(&'a TrustedReadRoot, &'a Path)> {
        self.roots
            .iter()
            .find_map(|(name, root)| {
                path.strip_prefix(name)
                    .ok()
                    .map(|relative| (root, relative))
            })
            .ok_or_else(denied)
    }

    pub(super) async fn file(&self, path: &Path) -> Result<OpenWorkspaceFile> {
        #[cfg(target_os = "linux")]
        {
            let (root, relative) = self.root(path)?;
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("share-fs-before-open", &path.to_string_lossy()).await;
            let file = root
                .regular(nonempty(relative))
                .map_err(|e| open_error(path, e))?;
            let size = file.metadata()?.len();
            Ok(OpenWorkspaceFile {
                file: tokio::fs::File::from_std(file),
                display_path: path.to_path_buf(),
                size,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Err(denied())
        }
    }

    pub(super) async fn directory(&self, path: &Path) -> Result<ReadDirectory> {
        #[cfg(target_os = "linux")]
        {
            let (root, relative) = self.root(path)?;
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("share-fs-before-open", &path.to_string_lossy()).await;
            let handle = root
                .directory(nonempty(relative))
                .map_err(|e| open_error(path, e))?;
            Ok(ReadDirectory { handle })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Err(denied())
        }
    }

    pub(super) async fn repository(&self, dir: &Path) -> Result<ReadRepository> {
        let directory = self.directory(dir).await?;
        let root = git_root(&directory.path()).await?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let handle = TrustedReadRoot::open_absolute(&root).map_err(|_| denied())?;
            // Git discovery reports a name. Bind that name without following
            // links, and prove it contains the directory we actually opened.
            let relative = dir.strip_prefix(&root).map_err(|_| denied())?;
            let from_repo = handle.directory(nonempty(relative)).map_err(|_| denied())?;
            let a = from_repo.0.metadata()?;
            let b = directory.handle.0.metadata()?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return Err(denied());
            }
            Ok(ReadRepository {
                root,
                directory: ReadDirectory { handle },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Err(denied())
        }
    }
}

#[cfg(target_os = "linux")]
fn nonempty(path: &Path) -> &Path {
    if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    }
}

#[cfg(target_os = "linux")]
fn open_error(path: &Path, e: std::io::Error) -> CalmError {
    match e.raw_os_error() {
        Some(nix::libc::ELOOP | nix::libc::EXDEV | nix::libc::ENOTDIR) => denied(),
        Some(nix::libc::ENXIO | nix::libc::ENODEV) => {
            CalmError::BadRequest(format!("path {} is not a regular file", path.display()))
        }
        None if e.kind() == ErrorKind::InvalidInput => {
            CalmError::BadRequest(format!("path {} is not a regular file", path.display()))
        }
        _ => map_io_err(path, e),
    }
}

pub(super) struct ReadDirectory {
    #[cfg(target_os = "linux")]
    handle: TrustedReadRoot,
}

impl ReadDirectory {
    /// Linux procfs descriptor links keep directory reads and child Git processes
    /// attached to the live handle, even after its original name is replaced.
    pub(super) fn path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.handle.0.as_raw_fd()
            ))
        }
        #[cfg(not(target_os = "linux"))]
        unreachable!("scoped host reads require secure directory handles")
    }
}

pub(super) struct ReadRepository {
    pub(super) root: PathBuf,
    directory: ReadDirectory,
}
impl ReadRepository {
    pub(super) fn path(&self) -> PathBuf {
        self.directory.path()
    }
}
