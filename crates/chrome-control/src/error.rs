use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use crate::PageInfo;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The browser process could not be started.
    #[error("cannot start {binary}: {source}")]
    Spawn {
        binary: PathBuf,
        source: std::io::Error,
    },
    /// A live Chrome holds the profile's `SingletonLock`: the new process
    /// handed its command line to that holder and exited.
    #[error("the profile {profile} is held by another running Chrome (exit {status})")]
    ProfileBusy {
        profile: PathBuf,
        status: ExitStatus,
    },
    /// The browser exited during launch for another reason.
    #[error("Chrome exited during launch ({status})")]
    LaunchExited { status: ExitStatus },
    /// The browser is gone: its process exited or it closed the CDP pipe.
    #[error("Chrome has exited")]
    Exited,
    /// Waiting for the browser process failed; something else reaped it.
    #[error("waiting for Chrome failed: {0}")]
    Wait(String),
    /// No page target reports `document.visibilityState == "visible"`; names
    /// every page, including those of unknown visibility.
    #[error("no visible page; pages: {pages:?}")]
    NoVisiblePage { pages: Vec<PageInfo> },
    /// More than one page may be visible: several are (a second window or a
    /// popup), or one is beside a page of unknown visibility. Names the
    /// visible pages and those of unknown visibility.
    #[error("more than one visible page: {pages:?}")]
    AmbiguousPage { pages: Vec<PageInfo> },
    /// Chrome answered a CDP call with an error, or a script threw.
    #[error("CDP {method} failed: {message}")]
    Cdp { method: String, message: String },
    /// Navigation failed before a document committed (for example a DNS error).
    #[error("navigation to {url} failed: {error}")]
    Navigation { url: String, error: String },
    /// A CDP call or the launch handshake did not finish in time.
    #[error("{what} did not finish within {after:?}")]
    Timeout { what: String, after: Duration },
}

pub type Result<T> = std::result::Result<T, Error>;
