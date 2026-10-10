//! Launch and drive one Chrome on a Wayland display.
//!
//! [`Chrome::launch`] starts Chrome with a persistent profile on the given
//! Wayland display and talks the Chrome DevTools Protocol over
//! `--remote-debugging-pipe` (fds 3 and 4, NUL-delimited JSON), so no TCP port
//! exists. The child environment is an explicit allowlist. The handle reads
//! and navigates the one visible page and owns the browser's teardown:
//!
//! - the browser runs in its own process group with `PR_SET_PDEATHSIG(SIGKILL)`,
//!   forked from one launcher thread that lives as long as the process;
//! - [`Chrome::stop`] and dropping the handle signal that group, and only while
//!   the browser is unreaped, so its id cannot name another process;
//! - nothing here signals a process it did not spawn: no pid files, no `/proc`
//!   scans, no start-up reaping.
//!
//! The crate knows nothing about Wayland internals or Neige.
#![cfg(target_os = "linux")]

mod cdp;
mod chrome;
mod error;
mod launch;
mod page;
mod process;

pub use chrome::Chrome;
pub use error::{Error, Result};
pub use launch::{LaunchConfig, WaylandEnv};
pub use page::{Navigated, PageInfo, PageText};
