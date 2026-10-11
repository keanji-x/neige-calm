//! The `chrome` app: one Chrome, launched on demand inside its sandbox, owned
//! by one task.
//!
//! The task holds the only `Chrome` handle, so it alone launches, drives and
//! stops it, and nothing can use a browser that is being stopped. Callers
//! send it requests; the app's state (running, and the browser pid whose
//! windows are its windows) is published on a watch channel for readers that
//! must not wait behind a navigation.

use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrome_control::{Chrome, LaunchConfig, Navigated, PageText, WaylandEnv, Wrapper};
use sandbox::{Etc, Policy};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// What a request to the app can fail with.
#[derive(Debug)]
pub enum AppError {
    /// The app is not running, and the request does not start it.
    NotRunning,
    /// The plugin is stopping.
    ShuttingDown,
    /// Chrome's sandbox could not be set up, so Chrome was not started.
    Sandbox(String),
    /// Chrome failed or refused.
    Chrome(chrome_control::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotRunning => write!(f, "Chrome is not running; page_open starts it"),
            AppError::ShuttingDown => write!(f, "the desktop plugin is stopping"),
            AppError::Sandbox(e) => write!(f, "Chrome's sandbox cannot be set up: {e}"),
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

/// Where Chrome sees its files inside the sandbox. Fixed, so they never
/// mirror (or reveal) host paths.
pub const INSIDE_CHROME_DIR: &str = "/opt/chrome";
pub const INSIDE_PROFILE: &str = "/data/profile";
pub const INSIDE_HOME: &str = "/data/home";
pub const INSIDE_RUNTIME_DIR: &str = "/run/wayland";
pub const INSIDE_PATH: &str = "/usr/bin";

/// How to launch Chrome. The paths are host paths.
#[derive(Debug, Clone)]
pub struct ChromeSetup {
    pub binary: PathBuf,
    pub bwrap: PathBuf,
    /// The plugin's data directory, and the plugin process's `HOME` when it
    /// has one: Chrome's install directory may not contain either, since the
    /// sandbox shows that directory to Chrome.
    pub data_dir: PathBuf,
    pub user_home: Option<PathBuf>,
    pub profile_dir: PathBuf,
    pub home_dir: PathBuf,
    pub wayland_socket: PathBuf,
    pub size: (u32, u32),
    pub proxy_server: Option<String>,
}

impl ChromeSetup {
    /// The real Chrome binary (links resolved), so its directory holds
    /// Chrome's files rather than a link to them. That directory is shown to
    /// Chrome, so it may not be, or contain, the plugin's data directory or
    /// `user_home`. Checked at `initialize` and again at every launch.
    pub fn install(&self) -> Result<PathBuf, String> {
        let binary = std::fs::canonicalize(&self.binary)
            .map_err(|e| format!("chrome_binary {}: {e}", self.binary.display()))?;
        let Some(chrome_dir) = binary.parent().filter(|_| binary.file_name().is_some()) else {
            return Err(format!("chrome_binary {} names no file", binary.display()));
        };
        for held in std::iter::once(&self.data_dir).chain(&self.user_home) {
            // Compared as real paths; one that cannot be resolved is compared as given.
            let held = std::fs::canonicalize(held).unwrap_or_else(|_| held.clone());
            if held.starts_with(chrome_dir) {
                return Err(format!(
                    "chrome_binary {} lies in {}, which holds {}: the sandbox shows that \
                     directory to Chrome. Install Chrome in a directory of its own",
                    self.binary.display(),
                    chrome_dir.display(),
                    held.display()
                ));
            }
        }
        Ok(binary)
    }

    /// Chrome's launch inside a bwrap sandbox that shows it only the
    /// directory holding its binary (read-only), its profile and home
    /// (read-write), the compositor's Wayland socket, and the sandbox's
    /// system base and `/etc` allowlist. It shares the host's network.
    /// `PATH` inside is `/usr/bin`; the rest of the environment is
    /// chrome-control's allowlist.
    ///
    /// The profile and home are made private (0700) first, created if
    /// missing: bwrap binds only paths that exist.
    fn launch_config(&self) -> Result<LaunchConfig, String> {
        for dir in [&self.profile_dir, &self.home_dir] {
            crate::layout::private_dir(dir)
                .map_err(|e| format!("prepare {}: {e}", dir.display()))?;
        }
        let binary = self.install()?;
        let (Some(chrome_dir), Some(name)) = (binary.parent(), binary.file_name()) else {
            unreachable!("install() returns a file with a parent directory");
        };
        let socket = self
            .wayland_socket
            .file_name()
            .expect("the compositor's socket path names a file");
        let inside_socket = Path::new(INSIDE_RUNTIME_DIR).join(socket);
        let mut policy = Policy::new(&self.bwrap)
            .and_then(|p| p.read_only(chrome_dir, INSIDE_CHROME_DIR))
            .and_then(|p| p.read_write(&self.profile_dir, INSIDE_PROFILE))
            .and_then(|p| p.read_write(&self.home_dir, INSIDE_HOME))
            .and_then(|p| p.socket(&self.wayland_socket, &inside_socket))
            // The host's PATH names directories the sandbox does not have.
            .and_then(|p| p.env("PATH", INSIDE_PATH))
            .map_err(|e| e.to_string())?;
        for entry in Etc::ALL {
            policy = policy.etc(entry);
        }
        Ok(LaunchConfig {
            binary: Path::new(INSIDE_CHROME_DIR).join(name),
            profile_dir: INSIDE_PROFILE.into(),
            home_dir: INSIDE_HOME.into(),
            wayland: WaylandEnv {
                display: socket.into(),
                runtime_dir: INSIDE_RUNTIME_DIR.into(),
            },
            size: self.size,
            proxy_server: self.proxy_server.clone(),
            wrapper: Some(Wrapper {
                program: policy.program().to_path_buf(),
                args: policy.args(),
            }),
        })
    }
}

/// Fails unless `bwrap` is an executable file: Chrome never runs outside its
/// sandbox, so the plugin does not start without one.
pub fn require_bwrap(bwrap: &Path) -> Result<(), String> {
    match std::fs::metadata(bwrap) {
        Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => Ok(()),
        Ok(_) => Err(format!(
            "config bwrap_binary {} is not an executable file; Chrome runs only inside \
             a bubblewrap sandbox",
            bwrap.display()
        )),
        Err(e) => Err(format!(
            "config bwrap_binary {}: {e}; Chrome runs only inside a bubblewrap sandbox \
             (install the bubblewrap package)",
            bwrap.display()
        )),
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
                // Unpublish the pid first: dropping the handle reaps the
                // process, after which its pid may name another one.
                state.send_replace(AppState { pid: None });
                chrome = None;
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
    // No retry on `ProfileBusy`: inside the sandbox Chrome is pid 2 with a
    // private /tmp, so it takes over a lock left by another sandbox instead
    // of handing off to it. Only the `http.sock` gate keeps two desktops,
    // and so two Chromes, off one profile (README, "One profile").
    let config = setup.launch_config().map_err(AppError::Sandbox)?;
    let browser = tokio::select! {
        launched = Chrome::launch(config) => launched.map_err(AppError::Chrome)?,
        () = shutdown.cancelled() => return Err(AppError::ShuttingDown),
    };
    let pid = browser.pid();
    tracing::info!(pid, "Chrome launched");
    *chrome = Some(browser);
    state.send_replace(AppState { pid: Some(pid) });
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Chrome sees fixed paths, and the sandbox binds exactly its install
    /// (resolved through a link), profile, home and Wayland socket.
    #[test]
    fn chrome_sees_fixed_paths_and_only_its_own_binds() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("chrome-linux64");
        std::fs::create_dir(&install).unwrap();
        std::fs::write(install.join("chrome"), b"").unwrap();
        let link = tmp.path().join("chrome");
        std::os::unix::fs::symlink(install.join("chrome"), &link).unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let socket = run.join("wayland-0");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let (profile, home) = (tmp.path().join("profiles/chrome"), tmp.path().join("home"));
        let setup = ChromeSetup {
            binary: link,
            bwrap: "/usr/bin/bwrap".into(),
            data_dir: tmp.path().join("data"),
            user_home: Some(tmp.path().join("user")),
            profile_dir: profile.clone(),
            home_dir: home.clone(),
            wayland_socket: socket.clone(),
            size: (1280, 800),
            proxy_server: Some("http://127.0.0.1:2080".into()),
        };
        let config = setup.launch_config().unwrap();
        assert_eq!(config.binary, Path::new("/opt/chrome/chrome"));
        assert_eq!(config.profile_dir, Path::new(INSIDE_PROFILE));
        assert_eq!(config.home_dir, Path::new(INSIDE_HOME));
        assert_eq!(
            config.wayland,
            WaylandEnv {
                display: "wayland-0".into(),
                runtime_dir: INSIDE_RUNTIME_DIR.into()
            }
        );
        assert_eq!(
            config.proxy_server.as_deref(),
            Some("http://127.0.0.1:2080")
        );
        for dir in [&profile, &home] {
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let wrapper = config.wrapper.expect("always sandboxed");
        assert_eq!(wrapper.program, Path::new("/usr/bin/bwrap"));
        let args = &wrapper.args;
        let triple = |flag: &str, host: &Path, inside: &str| {
            let want: [OsString; 3] = [flag.into(), host.into(), inside.into()];
            assert!(
                args.windows(3).any(|w| w == want),
                "{flag} {} {inside} in {args:?}",
                host.display()
            );
        };
        triple("--ro-bind", &install, INSIDE_CHROME_DIR);
        triple("--bind", &profile, INSIDE_PROFILE);
        triple("--bind", &home, INSIDE_HOME);
        triple("--ro-bind", &socket, "/run/wayland/wayland-0");
        triple("--setenv", Path::new("PATH"), INSIDE_PATH);
        for entry in Etc::ALL {
            triple("--ro-bind-try", Path::new(entry.path()), entry.path());
        }
        let host_paths = args
            .iter()
            .filter(|a| Path::new(a).starts_with(tmp.path()))
            .count();
        assert_eq!(host_paths, 4, "nothing else of the host: {args:?}");
        assert_eq!(args.last().map(OsString::as_os_str), Some("--".as_ref()));
    }

    /// Chrome's install directory is shown to Chrome, so one that holds the
    /// plugin's data or the user's home is refused, before any launch.
    #[test]
    fn an_install_directory_holding_the_data_or_home_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let socket = run.join("wayland-0");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let home = tmp.path().join("user");
        let data = home.join("plugins/desktop");
        std::fs::create_dir_all(&data).unwrap();
        let setup = |binary: PathBuf, user_home: Option<PathBuf>| ChromeSetup {
            binary,
            bwrap: "/usr/bin/bwrap".into(),
            data_dir: data.clone(),
            user_home,
            profile_dir: data.join("profiles/chrome"),
            home_dir: data.join("home"),
            wayland_socket: socket.clone(),
            size: (1280, 800),
            proxy_server: None,
        };
        // In the home itself, and in an ancestor of the data dir.
        std::fs::write(home.join("chrome"), b"").unwrap();
        std::fs::write(home.join("plugins/chrome"), b"").unwrap();
        for binary in [home.join("chrome"), home.join("plugins/chrome")] {
            for user_home in [Some(home.clone()), None] {
                let err = setup(binary.clone(), user_home)
                    .launch_config()
                    .unwrap_err();
                assert!(err.contains("directory of its own"), "{err}");
            }
        }
        // The data dir is outside this install; a home below it is not.
        let elsewhere = tmp.path().join("opt");
        std::fs::create_dir_all(elsewhere.join("me")).unwrap();
        std::fs::write(elsewhere.join("chrome"), b"").unwrap();
        assert!(
            setup(elsewhere.join("chrome"), None)
                .launch_config()
                .is_ok()
        );
        let err = setup(elsewhere.join("chrome"), Some(elsewhere.join("me")))
            .launch_config()
            .unwrap_err();
        assert!(err.contains("directory of its own"), "{err}");
    }

    #[test]
    fn the_plugin_requires_an_executable_bwrap() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = require_bwrap(&tmp.path().join("bwrap")).unwrap_err();
        assert!(missing.contains("bwrap_binary"), "{missing}");
        let plain = tmp.path().join("plain");
        std::fs::write(&plain, b"").unwrap();
        assert!(
            require_bwrap(&plain)
                .unwrap_err()
                .contains("not an executable")
        );
        assert!(require_bwrap(tmp.path()).is_err());
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(require_bwrap(&plain), Ok(()));
    }
}
