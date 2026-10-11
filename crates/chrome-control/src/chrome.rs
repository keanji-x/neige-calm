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
/// How long [`Chrome::stop`] waits for the browser to close after asking it
/// over CDP: an orderly shutdown in which the network service writes its
/// cookies to disk.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// How long [`Chrome::stop`] waits after SIGTERM, when the browser did not
/// close on request, before the group's SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(2);

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
/// asks the browser to shut down first. If the owning process dies, SIGKILL included,
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
    ///
    /// Only an unwrapped launch reports `ProfileBusy`. Behind a
    /// [`Wrapper`](crate::Wrapper) the exit code is the wrapper's, which
    /// proves no handoff (a sandbox can also hide the holder), so an early
    /// exit there is always [`Error::LaunchExited`].
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
            Err(Error::Exited) => {
                let busy_profile = config.wrapper.is_none().then_some(config.profile_dir);
                Err(launch_exit(&process, busy_profile).await)
            }
            Err(error) => Err(error),
        }
    }

    /// Process id of the launched process (also its process-group id): the
    /// browser, or its [`Wrapper`](crate::Wrapper) when there is one.
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /// Every page target with its visibility (`Unknown` when the page did not
    /// answer within two seconds).
    pub async fn pages(&self) -> Result<Vec<PageInfo>> {
        page::list(&self.cdp).await
    }

    /// Navigates the one visible page and returns its url and title after the
    /// new document's load event, or `loaded: false` when the load did not come
    /// in time. `timeout` bounds the whole call (`Duration::MAX`: no limit);
    /// when even the url and title cannot be read within it, the result is
    /// [`Error::Timeout`].
    ///
    /// `loaded: false` and `Timeout` do not mean nothing happened: when finding
    /// the visible page used up most of the budget, `Page.navigate` may still
    /// have been sent, and the navigation may go on after this returns.
    pub async fn navigate(&self, url: &str, timeout: Duration) -> Result<Navigated> {
        page::navigate(&self.cdp, url, timeout).await
    }

    /// Url, title and `document.body.innerText` of the one visible page, the
    /// text cut inside the page to at most `max_text_units` UTF-16 code units
    /// (`PageText::text_truncated` says whether it was).
    ///
    /// Every page's visibility is checked side by side, each with a two-second
    /// limit. A page that does not answer (a JS dialog, a busy main thread) may
    /// be visible, so beside it even a single visible page is ambiguous:
    /// [`Error::AmbiguousPage`] names the visible and unanswered pages.
    pub async fn read_page(&self, max_text_units: u32) -> Result<PageText> {
        page::read(&self.cdp, max_text_units).await
    }

    /// Resolves when the browser process has exited.
    pub async fn exited(&self) -> Result<ExitStatus> {
        self.process.exited().await
    }

    /// Stops the browser in order and returns its exit status:
    /// 1. asks it over CDP to close (`Browser.close`), which shuts Chrome down
    ///    the way closing its last window does, cookies written to disk
    ///    (SIGTERM alone ends it within milliseconds, before they are), and
    ///    waits up to five seconds for it to exit;
    /// 2. if it has not, sends SIGTERM to the browser process alone and waits
    ///    two more seconds;
    /// 3. sends SIGKILL to the whole group, which ends any helper still in it.
    ///
    /// Every signal goes to a browser this handle has not reaped yet.
    pub async fn stop(self) -> Result<ExitStatus> {
        let closed = async {
            // A reply, an error or a closed pipe: either way, wait for the exit.
            let _ = self
                .cdp
                .call("Browser.close", json!({}), None, CLOSE_GRACE)
                .await;
            self.process.exited().await
        };
        if tokio::time::timeout(CLOSE_GRACE, closed).await.is_err() {
            self.process.signal_leader(libc::SIGTERM);
            let _ = tokio::time::timeout(TERM_GRACE, self.process.exited()).await;
        }
        self.process.signal_group(libc::SIGKILL);
        self.process.exited().await
    }
}

/// Classifies a browser that closed its CDP pipe during launch. `profile`
/// is `Some` only for an unwrapped launch, the one whose exit code is
/// Chrome's own.
async fn launch_exit(process: &ChildProcess, profile: Option<PathBuf>) -> Error {
    let busy = |status: &ExitStatus| {
        status
            .code()
            .is_some_and(|c| PROFILE_BUSY_EXIT_CODES.contains(&c))
    };
    match (
        tokio::time::timeout(EXIT_GRACE, process.exited()).await,
        profile,
    ) {
        (Ok(Ok(status)), Some(profile)) if busy(&status) => Error::ProfileBusy { profile, status },
        (Ok(Ok(status)), _) => Error::LaunchExited { status },
        (Ok(Err(error)), _) => error,
        // The pipe closed but the process lives on; dropping it kills the group.
        (Err(_), _) => Error::Exited,
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
