//! A bubblewrap (`bwrap`) sandbox described by a typed policy.
//!
//! A [`Policy`] says what a program sees of the host; [`Policy::args`] turns
//! it into bwrap's options, which the program and its arguments follow.
//! Every policy:
//!
//! - unshares every namespace bwrap can (user, mount, pid, ipc, uts, cgroup)
//!   except the network, which the program shares with the host;
//! - starts the program in a new session (`--new-session`, so it cannot push
//!   input into the caller's terminal), and ends the sandbox when bwrap's
//!   parent dies (`--die-with-parent`). The program and everything it starts
//!   live in the sandbox's pid namespace, which the kernel empties when the
//!   namespace's init exits: killing bwrap ends them all;
//! - shows a fixed system base: `/usr` read-only with the merged-usr links
//!   `/bin`, `/sbin`, `/lib` and `/lib64`, a fresh `/proc`, a minimal `/dev`,
//!   and private tmpfs mounts on `/tmp` and `/dev/shm`;
//! - shows from the host's `/etc` only the entries [`Etc`] names, a closed
//!   set, read-only.
//!
//! The caller adds read-only, read-write and socket binds, each at a path of
//! its choosing inside, and environment variables. Nothing else of the host
//! is mounted: the root is an empty tmpfs.
//!
//! Paths are checked lexically. The host paths come from the caller, who must
//! own and trust them: bwrap follows a symlink in them.
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

/// The host's `/etc` entries a policy may show, read-only, at the same path.
/// A closed set: nothing else of `/etc` is ever mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Etc {
    /// `/etc/fonts`: fontconfig's configuration.
    Fonts,
    /// `/etc/ssl/certs`: the system's CA certificates.
    Certificates,
    /// `/etc/resolv.conf`: the DNS resolver.
    ResolvConf,
    /// `/etc/hosts`: static host names.
    Hosts,
    /// `/etc/nsswitch.conf`: where name lookups go.
    NsSwitch,
    /// `/etc/localtime`: the time zone.
    Localtime,
    /// `/etc/ld.so.cache`: the dynamic linker's index of `/usr`'s libraries.
    LdSoCache,
}

impl Etc {
    pub const ALL: [Etc; 7] = [
        Etc::Fonts,
        Etc::Certificates,
        Etc::ResolvConf,
        Etc::Hosts,
        Etc::NsSwitch,
        Etc::Localtime,
        Etc::LdSoCache,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Etc::Fonts => "/etc/fonts",
            Etc::Certificates => "/etc/ssl/certs",
            Etc::ResolvConf => "/etc/resolv.conf",
            Etc::Hosts => "/etc/hosts",
            Etc::NsSwitch => "/etc/nsswitch.conf",
            Etc::Localtime => "/etc/localtime",
            Etc::LdSoCache => "/etc/ld.so.cache",
        }
    }
}

/// Paths inside the sandbox that its system base owns: a bind may not land
/// on or below them (and `/etc` is shown only through [`Etc`]).
const RESERVED_INSIDE: [&str; 9] = [
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/proc", "/dev", "/tmp", "/etc",
];

/// Why a policy refused a step.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{what} path {path} must be absolute, without `..`")]
    BadPath { what: &'static str, path: PathBuf },
    #[error(
        "{0} may not be bound: the host's root and /etc reach the sandbox only through \
         its system base and its /etc allowlist"
    )]
    HostRefused(PathBuf),
    #[error("{0} is on or below a path the sandbox's system base owns")]
    Reserved(PathBuf),
    #[error("{0} is mounted twice")]
    Duplicate(PathBuf),
    #[error("{0} is not a socket")]
    NotASocket(PathBuf),
    #[error("cannot inspect {path}: {source}")]
    Inspect { path: PathBuf, source: io::Error },
    #[error("{0:?} is not an environment variable name")]
    EnvName(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Bind {
    ReadOnly,
    ReadWrite,
    Socket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    kind: Bind,
    host: PathBuf,
    inside: PathBuf,
}

/// What a sandboxed program sees of the host. Build it with the methods
/// below, then run `program()` with `args()` followed by the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    bwrap: PathBuf,
    etc: BTreeSet<Etc>,
    mounts: Vec<Mount>,
    env: Vec<(String, OsString)>,
}

impl Policy {
    /// A policy that shows only the system base. `bwrap` is the absolute
    /// path of the bubblewrap executable.
    pub fn new(bwrap: impl Into<PathBuf>) -> Result<Self> {
        let bwrap = clean("bwrap", bwrap.into())?;
        Ok(Self {
            bwrap,
            etc: BTreeSet::new(),
            mounts: Vec::new(),
            env: Vec::new(),
        })
    }

    /// Shows the host's `entry` read-only at the same path. A host without
    /// that entry gives a sandbox without it.
    pub fn etc(mut self, entry: Etc) -> Self {
        self.etc.insert(entry);
        self
    }

    /// Shows the host path `host` read-only at `inside`.
    pub fn read_only(self, host: impl Into<PathBuf>, inside: impl Into<PathBuf>) -> Result<Self> {
        self.mount(Bind::ReadOnly, host.into(), inside.into())
    }

    /// Shows the host path `host` read-write at `inside`.
    pub fn read_write(self, host: impl Into<PathBuf>, inside: impl Into<PathBuf>) -> Result<Self> {
        self.mount(Bind::ReadWrite, host.into(), inside.into())
    }

    /// Shows the Unix socket at `host` at `inside`, so the program can
    /// connect to it. `host` must be a socket now.
    pub fn socket(self, host: impl Into<PathBuf>, inside: impl Into<PathBuf>) -> Result<Self> {
        let host = host.into();
        let meta = std::fs::metadata(&host).map_err(|source| Error::Inspect {
            path: host.clone(),
            source,
        })?;
        if !meta.file_type().is_socket() {
            return Err(Error::NotASocket(host));
        }
        self.mount(Bind::Socket, host, inside.into())
    }

    /// Sets `key` to `value` in the program's environment, on top of the
    /// environment bwrap itself was started with.
    pub fn env(mut self, key: &str, value: impl Into<OsString>) -> Result<Self> {
        if key.is_empty() || key.contains(['=', '\0']) {
            return Err(Error::EnvName(key.to_owned()));
        }
        self.env.push((key.to_owned(), value.into()));
        Ok(self)
    }

    fn mount(mut self, kind: Bind, host: PathBuf, inside: PathBuf) -> Result<Self> {
        let host = clean("host", host)?;
        let inside = clean("inside", inside)?;
        if host == Path::new("/") || host.starts_with("/etc") {
            return Err(Error::HostRefused(host));
        }
        if inside == Path::new("/") || RESERVED_INSIDE.iter().any(|r| inside.starts_with(r)) {
            return Err(Error::Reserved(inside));
        }
        if self.mounts.iter().any(|m| m.inside == inside) {
            return Err(Error::Duplicate(inside));
        }
        self.mounts.push(Mount { kind, host, inside });
        Ok(self)
    }

    /// The bubblewrap executable to run.
    pub fn program(&self) -> &Path {
        &self.bwrap
    }

    /// bwrap's options, ending with `--`: the program and its arguments go
    /// right after them.
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/sbin",
            "/sbin",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/dev/shm",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        for entry in &self.etc {
            args.extend(["--ro-bind-try", entry.path(), entry.path()].map(OsString::from));
        }
        for mount in &self.mounts {
            let flag = match mount.kind {
                Bind::ReadOnly | Bind::Socket => "--ro-bind",
                Bind::ReadWrite => "--bind",
            };
            args.push(flag.into());
            args.push(mount.host.clone().into());
            args.push(mount.inside.clone().into());
        }
        for (key, value) in &self.env {
            args.push("--setenv".into());
            args.push(key.into());
            args.push(value.clone());
        }
        args.push("--".into());
        args
    }
}

/// `path` if it is absolute and has no `..` (`Path::components` already
/// drops `.` and repeated slashes).
fn clean(what: &'static str, path: PathBuf) -> Result<PathBuf> {
    let ok = path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if ok {
        Ok(path.components().collect())
    } else {
        Err(Error::BadPath { what, path })
    }
}

#[cfg(test)]
mod tests;
