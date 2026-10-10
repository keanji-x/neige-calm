//! The `chrome` app: one Chrome, launched on demand, owned by one task.
//!
//! The task holds the only `Chrome` handle, so it alone launches, drives and
//! stops it, and nothing can use a browser that is being stopped. Callers
//! send it requests; the app's state (running, and the browser pid whose
//! windows are its windows) is published on a watch channel for readers that
//! must not wait behind a navigation.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use chrome_control::{Chrome, LaunchConfig, Navigated, PageText, WaylandEnv};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// Launch attempts while a live Chrome still holds the profile (a restart
/// overlapping the old process's kill). Each refused attempt opens a window
/// in that holder, so the bound stays small and the backoff short.
pub const LAUNCH_ATTEMPTS: u32 = 5;
pub const LAUNCH_BACKOFF: Duration = Duration::from_millis(300);

/// What a request to the app can fail with.
#[derive(Debug)]
pub enum AppError {
    /// The app is not running, and the request does not start it.
    NotRunning,
    /// The plugin is stopping.
    ShuttingDown,
    /// Chrome failed or refused.
    Chrome(chrome_control::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotRunning => write!(f, "Chrome is not running; page_open starts it"),
            AppError::ShuttingDown => write!(f, "the desktop plugin is stopping"),
            AppError::Chrome(e) => write!(f, "{e}"),
        }
    }
}

/// The app's published state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppState {
    /// The browser process id while Chrome runs; its windows are the app's.
    pub pid: Option<u32>,
}

enum Request {
    Start(oneshot::Sender<Result<u32, AppError>>),
    Navigate {
        url: String,
        timeout: Duration,
        reply: oneshot::Sender<Result<Navigated, AppError>>,
    },
    Read {
        max_text_units: u32,
        reply: oneshot::Sender<Result<PageText, AppError>>,
    },
}

/// Handle to the app task. Cheap to clone.
#[derive(Clone)]
pub struct ChromeApp {
    requests: mpsc::Sender<Request>,
    state: watch::Receiver<AppState>,
}

/// How to launch Chrome.
#[derive(Debug, Clone)]
pub struct ChromeSetup {
    pub binary: PathBuf,
    pub profile_dir: PathBuf,
    pub home_dir: PathBuf,
    pub wayland_socket: PathBuf,
    pub size: (u32, u32),
    pub proxy_server: Option<String>,
}

impl ChromeSetup {
    fn launch_config(&self) -> LaunchConfig {
        LaunchConfig {
            binary: self.binary.clone(),
            profile_dir: self.profile_dir.clone(),
            home_dir: self.home_dir.clone(),
            wayland: WaylandEnv {
                display: self.wayland_socket.clone(),
                runtime_dir: self
                    .wayland_socket
                    .parent()
                    .expect("the socket lives in a run dir")
                    .to_path_buf(),
            },
            size: self.size,
            proxy_server: self.proxy_server.clone(),
        }
    }
}

impl ChromeApp {
    /// Starts the app task. It ends when `shutdown` is cancelled, after it
    /// stopped Chrome (`Chrome::stop`: SIGTERM, 5 s, then the group's SIGKILL).
    pub fn spawn(
        setup: ChromeSetup,
        shutdown: CancellationToken,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (requests, inbox) = mpsc::channel(16);
        let (state_tx, state) = watch::channel(AppState { pid: None });
        let task = tokio::spawn(run(setup, inbox, state_tx, shutdown));
        (Self { requests, state }, task)
    }

    pub fn state(&self) -> AppState {
        *self.state.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<AppState> {
        self.state.clone()
    }

    /// Starts Chrome unless it runs; the browser's pid.
    pub async fn start(&self) -> Result<u32, AppError> {
        self.ask(Request::Start).await
    }

    /// Starts Chrome unless it runs, then navigates its visible page.
    pub async fn navigate(&self, url: String, timeout: Duration) -> Result<Navigated, AppError> {
        self.ask(|reply| Request::Navigate {
            url,
            timeout,
            reply,
        })
        .await
    }

    /// Reads the visible page of a running Chrome; does not start it.
    pub async fn read(&self, max_text_units: u32) -> Result<PageText, AppError> {
        self.ask(|reply| Request::Read {
            max_text_units,
            reply,
        })
        .await
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, AppError>>) -> Request,
    ) -> Result<T, AppError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_| AppError::ShuttingDown)?;
        answer.await.unwrap_or(Err(AppError::ShuttingDown))
    }
}

async fn run(
    setup: ChromeSetup,
    mut inbox: mpsc::Receiver<Request>,
    state: watch::Sender<AppState>,
    shutdown: CancellationToken,
) {
    let mut chrome: Option<Chrome> = None;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            status = exited(&chrome) => {
                tracing::info!(?status, "Chrome exited; the app is not running");
                // Dropping a handle whose browser exited reaps it; its group
                // is gone with it.
                chrome = None;
                state.send_replace(AppState { pid: None });
            }
            request = inbox.recv() => match request {
                None => break,
                Some(request) => serve(request, &setup, &mut chrome, &state, &shutdown).await,
            },
        }
    }
    if let Some(chrome) = chrome.take() {
        state.send_replace(AppState { pid: None });
        let pid = chrome.pid();
        match chrome.stop().await {
            Ok(status) => tracing::info!(pid, ?status, "Chrome stopped"),
            Err(e) => tracing::warn!(pid, error = %e, "stopping Chrome failed"),
        }
    }
}

/// Resolves when the running browser exits; never without one.
async fn exited(chrome: &Option<Chrome>) -> chrome_control::Result<std::process::ExitStatus> {
    match chrome {
        Some(chrome) => chrome.exited().await,
        None => std::future::pending().await,
    }
}

async fn serve(
    request: Request,
    setup: &ChromeSetup,
    chrome: &mut Option<Chrome>,
    state: &watch::Sender<AppState>,
    shutdown: &CancellationToken,
) {
    match request {
        Request::Start(reply) => {
            let started = ensure(setup, chrome, state, shutdown).await;
            let _ = reply.send(started);
        }
        Request::Navigate {
            url,
            timeout,
            reply,
        } => {
            let result = match ensure(setup, chrome, state, shutdown).await {
                Err(e) => Err(e),
                Ok(_) => {
                    let browser = chrome.as_ref().expect("ensure leaves a browser");
                    guarded(browser, shutdown, browser.navigate(&url, timeout)).await
                }
            };
            let _ = reply.send(result);
        }
        Request::Read {
            max_text_units,
            reply,
        } => {
            let result = match chrome.as_ref() {
                None => Err(AppError::NotRunning),
                Some(browser) => {
                    guarded(browser, shutdown, browser.read_page(max_text_units)).await
                }
            };
            let _ = reply.send(result);
        }
    }
}

/// Runs one browser call, cut short by the browser's exit or the plugin's
/// shutdown.
async fn guarded<T>(
    browser: &Chrome,
    shutdown: &CancellationToken,
    call: impl Future<Output = chrome_control::Result<T>>,
) -> Result<T, AppError> {
    tokio::select! {
        result = call => result.map_err(AppError::Chrome),
        _ = browser.exited() => Err(AppError::Chrome(chrome_control::Error::Exited)),
        () = shutdown.cancelled() => Err(AppError::ShuttingDown),
    }
}

/// The running browser's pid, launching it first when there is none.
async fn ensure(
    setup: &ChromeSetup,
    chrome: &mut Option<Chrome>,
    state: &watch::Sender<AppState>,
    shutdown: &CancellationToken,
) -> Result<u32, AppError> {
    if let Some(browser) = chrome.as_ref() {
        return Ok(browser.pid());
    }
    let config = setup.launch_config();
    let launch = launch_with_retry(
        || Chrome::launch(config.clone()),
        LAUNCH_ATTEMPTS,
        LAUNCH_BACKOFF,
    );
    let browser = tokio::select! {
        launched = launch => launched.map_err(AppError::Chrome)?,
        () = shutdown.cancelled() => return Err(AppError::ShuttingDown),
    };
    let pid = browser.pid();
    tracing::info!(pid, "Chrome launched");
    *chrome = Some(browser);
    state.send_replace(AppState { pid: Some(pid) });
    Ok(pid)
}

/// Calls `launch` until it does not report a busy profile, at most `attempts`
/// times, `backoff` apart.
pub async fn launch_with_retry<T, F, Fut>(
    mut launch: F,
    attempts: u32,
    backoff: Duration,
) -> chrome_control::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = chrome_control::Result<T>>,
{
    let mut attempt = 1;
    loop {
        match launch().await {
            Err(chrome_control::Error::ProfileBusy { profile, status }) if attempt < attempts => {
                tracing::info!(attempt, profile = %profile.display(), %status,
                    "the profile is still held; retrying the launch");
                attempt += 1;
                tokio::time::sleep(backoff).await;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn busy() -> chrome_control::Error {
        chrome_control::Error::ProfileBusy {
            profile: "/p".into(),
            status: std::process::ExitStatus::from_raw(0),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_profile_is_retried_a_bounded_number_of_times() {
        let calls = AtomicU32::new(0);
        let result: chrome_control::Result<()> = launch_with_retry(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(busy()) }
            },
            LAUNCH_ATTEMPTS,
            LAUNCH_BACKOFF,
        )
        .await;
        assert!(matches!(
            result,
            Err(chrome_control::Error::ProfileBusy { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), LAUNCH_ATTEMPTS);
        assert_eq!(LAUNCH_ATTEMPTS, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_stops_at_success_and_never_retries_other_errors() {
        let calls = AtomicU32::new(0);
        let result = launch_with_retry(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n < 2 { Err(busy()) } else { Ok(n) } }
            },
            LAUNCH_ATTEMPTS,
            LAUNCH_BACKOFF,
        )
        .await;
        assert_eq!(result.unwrap(), 2);

        let calls = AtomicU32::new(0);
        let result: chrome_control::Result<()> = launch_with_retry(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(chrome_control::Error::Exited) }
            },
            LAUNCH_ATTEMPTS,
            LAUNCH_BACKOFF,
        )
        .await;
        assert!(matches!(result, Err(chrome_control::Error::Exited)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
