//! The browser handle.

use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use serde_json::json;

use crate::cdp::Cdp;
use crate::launch::{self, LaunchConfig};
use crate::page::{self, Navigated, PageInfo, PageText};
use crate::process::ChildProcess;
use crate::{Error, Result};

/// How long launch waits for the browser to answer its first CDP call.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long launch waits for the exit status after the CDP pipe closed.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Time between SIGTERM and SIGKILL in [`Chrome::stop`].
const STOP_GRACE: Duration = Duration::from_secs(1);

/// Exit codes of a browser that found its profile held by another browser
/// before it answered on the CDP pipe:
/// - 0: it handed its command line to the live holder
///   (`RESULT_CODE_NORMAL_EXIT_PROCESS_NOTIFIED`, which Chrome turns into a
///   normal exit; measured on Chrome for Testing 151.0.7922.34). A browser that
///   has not answered CDP yet has no other reason to exit normally.
/// - 21: `RESULT_CODE_PROFILE_IN_USE` (`chrome/common/chrome_result_codes.h`),
///   the lock names another host.
const PROFILE_BUSY_EXIT_CODES: [i32; 2] = [0, 21];

/// A running Chrome that this handle owns.
///
/// Dropping the handle SIGKILLs the browser's process group; [`Chrome::stop`]
/// stops it with SIGTERM first. If the owning process dies, SIGKILL included,
/// the browser process gets SIGKILL from `PR_SET_PDEATHSIG`.
pub struct Chrome {
    // Field order: the CDP pipe closes before the group is killed.
    cdp: Cdp,
    process: ChildProcess,
}

impl Chrome {
    /// Starts Chrome and waits until it answers on the CDP pipe.
    ///
    /// Must run inside a tokio runtime with IO and time enabled. When a live
    /// Chrome holds the profile, the new process hands its command line to
    /// that holder and exits; this returns [`Error::ProfileBusy`] at once,
    /// detected from the exit, without retrying: each attempt would open a
    /// window in the holder, so the caller owns the retry bound.
    pub async fn launch(config: LaunchConfig) -> Result<Chrome> {
        let spawned = launch::spawn(&config).await?;
        let cdp = Cdp::start(spawned.responses, spawned.commands);
        let process = spawned.process;
        let closer = cdp.closer();
        let mut exit = process.exit_watch();
        tokio::spawn(async move {
            // Ok or Err, the browser is gone (or unobservable): fail pending calls.
            let _ = exit.wait_for(Option::is_some).await;
            closer.close();
        });
        match cdp
            .call("Browser.getVersion", json!({}), None, LAUNCH_TIMEOUT)
            .await
        {
            Ok(_) => Ok(Chrome { cdp, process }),
            Err(Error::Exited) => Err(launch_exit(&process, config.profile_dir).await),
            Err(error) => Err(error),
        }
    }

    /// Process id of the browser process (also its process-group id).
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /// Every page target with its visibility.
    pub async fn pages(&self) -> Result<Vec<PageInfo>> {
        page::list(&self.cdp).await
    }

    /// Navigates the one visible page and returns its url and title after the
    /// new document's load event, or when `timeout` passes (`loaded: false`).
    pub async fn navigate(&self, url: &str, timeout: Duration) -> Result<Navigated> {
        page::navigate(&self.cdp, url, timeout).await
    }

    /// Url, title and `document.body.innerText` of the one visible page.
    pub async fn read_page(&self) -> Result<PageText> {
        page::read(&self.cdp).await
    }

    /// Resolves when the browser process has exited.
    pub async fn exited(&self) -> Result<ExitStatus> {
        self.process.exited().await
    }

    /// Sends SIGTERM to the browser's process group, waits up to one second
    /// for the browser to exit, then sends SIGKILL to the group (which also
    /// ends helpers still in it) and returns the browser's exit status.
    pub async fn stop(self) -> Result<ExitStatus> {
        self.process.signal_group(libc::SIGTERM);
        let _ = tokio::time::timeout(STOP_GRACE, self.process.exited()).await;
        self.process.signal_group(libc::SIGKILL);
        self.process.exited().await
    }
}

/// Classifies a browser that closed its CDP pipe during launch.
async fn launch_exit(process: &ChildProcess, profile: PathBuf) -> Error {
    match tokio::time::timeout(EXIT_GRACE, process.exited()).await {
        Ok(Ok(status))
            if status
                .code()
                .is_some_and(|c| PROFILE_BUSY_EXIT_CODES.contains(&c)) =>
        {
            Error::ProfileBusy { profile, status }
        }
        Ok(Ok(status)) => Error::LaunchExited { status },
        Ok(Err(error)) => error,
        // The pipe closed but the process lives on; dropping it kills the group.
        Err(_) => Error::Exited,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_handle_can_be_shared_across_tasks() {
        fn shareable<T: Send + Sync + 'static>() {}
        shareable::<super::Chrome>();
    }
}
