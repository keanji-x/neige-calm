//! Process-level tests with a fake Chrome (no real browser in CI).
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrome_control::{Chrome, Error, LaunchConfig, WaylandEnv};

const FAKE_CHROME: &str = env!("CARGO_BIN_EXE_chrome-control-fake-chrome");
const OWNER: &str = env!("CARGO_BIN_EXE_chrome-control-test-owner");
const DEATH_BOUND: Duration = Duration::from_secs(5);

struct Dirs {
    _root: tempfile::TempDir,
    profile: PathBuf,
    home: PathBuf,
    runtime: PathBuf,
}

fn dirs(mode: &str) -> Dirs {
    let root = tempfile::Builder::new()
        .prefix("chrome-control-")
        .tempdir()
        .unwrap();
    let profile = root.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    std::fs::write(profile.join("fake-chrome-mode"), mode).unwrap();
    Dirs {
        profile,
        home: root.path().join("home"),
        runtime: root.path().join("run"),
        _root: root,
    }
}

fn config(dirs: &Dirs) -> LaunchConfig {
    LaunchConfig {
        binary: FAKE_CHROME.into(),
        profile_dir: dirs.profile.clone(),
        home_dir: dirs.home.clone(),
        wayland: WaylandEnv {
            display: "wayland-test".into(),
            runtime_dir: dirs.runtime.clone(),
        },
        size: (800, 600),
    }
}

/// Starts the owner fixture with exactly `env`, returns it and the browser pid.
fn start_owner(dirs: &Dirs, env: &[(&str, &str)]) -> (Child, i32) {
    let mut owner = Command::new(OWNER)
        .env_clear()
        .envs(env.iter().copied())
        .arg(FAKE_CHROME)
        .arg(&dirs.profile)
        .arg(&dirs.home)
        .arg("wayland-test")
        .arg(&dirs.runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(owner.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid = line
        .trim()
        .strip_prefix("browser ")
        .unwrap_or_else(|| panic!("owner said {line:?}"))
        .parse()
        .unwrap();
    (owner, pid)
}

/// A process identity that survives pid reuse: (pid, start time in ticks).
#[derive(Debug, Clone, Copy)]
struct Proc {
    pid: i32,
    start: u64,
}

impl Proc {
    fn of(pid: i32) -> Proc {
        let start = stat(pid)
            .unwrap_or_else(|| panic!("process {pid} is not running"))
            .1;
        Proc { pid, start }
    }

    /// Running (not a zombie) and still the same process.
    fn alive(&self) -> bool {
        stat(self.pid)
            .is_some_and(|(state, start)| start == self.start && state != 'Z' && state != 'X')
    }

    fn dies_within(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        while Instant::now() < deadline {
            if !self.alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !self.alive()
    }
}

/// (state, start time) from /proc/<pid>/stat.
fn stat(pid: i32) -> Option<(char, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    // After ")": state is field 3 of stat(5), starttime is field 22.
    Some((fields[0].chars().next()?, fields[19].parse().ok()?))
}

/// The fake's helpers as (in_group, own_group, session).
fn helpers(profile: &Path) -> (Proc, Proc, Proc) {
    let path = profile.join("fake-chrome-helpers");
    let deadline = Instant::now() + DEATH_BOUND;
    let text = loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the fake never wrote its helper pids"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let pids: Vec<Proc> = text
        .split_whitespace()
        .map(|pid| Proc::of(pid.parse().unwrap()))
        .collect();
    (pids[0], pids[1], pids[2])
}

fn environ(pid: i32) -> BTreeMap<String, String> {
    std::fs::read(format!("/proc/{pid}/environ"))
        .unwrap()
        .split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = String::from_utf8(entry.to_vec()).unwrap();
            let (key, value) = entry.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

/// A3: the browser's environment is exactly the allowlist plus the config's
/// display and home, even when the owner holds secrets and its own display.
#[test]
fn browser_environment_is_exactly_the_allowlist() {
    let dirs = dirs("serve");
    let (mut owner, browser) = start_owner(
        &dirs,
        &[
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
            ("LC_TIME", "en_DK.UTF-8"),
            ("FONTCONFIG_FILE", "/etc/fonts/fonts.conf"),
            ("FONTCONFIG_PATH", "/etc/fonts"),
            ("HOME", "/home/the-owner"),
            ("WAYLAND_DISPLAY", "owner-display"),
            ("XDG_RUNTIME_DIR", "/run/owner"),
            ("NEIGE_SECRET_TOKEN", "hunter2"),
            ("OPENAI_API_KEY", "sk-test"),
            ("SSH_AUTH_SOCK", "/run/owner/ssh"),
        ],
    );
    let actual = environ(browser);
    owner.kill().unwrap();
    owner.wait().unwrap();
    let expected: BTreeMap<String, String> = [
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C.UTF-8"),
        ("LC_TIME", "en_DK.UTF-8"),
        ("FONTCONFIG_FILE", "/etc/fonts/fonts.conf"),
        ("FONTCONFIG_PATH", "/etc/fonts"),
        ("HOME", dirs.home.to_str().unwrap()),
        ("WAYLAND_DISPLAY", "wayland-test"),
        ("XDG_RUNTIME_DIR", dirs.runtime.to_str().unwrap()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert_eq!(actual, expected);
}

/// A4 mechanism 1: SIGKILL of the owner kills the browser process. Helpers
/// that left the browser's group survive: nothing here hunts for them; for
/// real Chrome their exit is Chrome's own behaviour (measured by the probe).
#[test]
fn owner_sigkill_kills_the_browser() {
    let dirs = dirs("helpers");
    let (mut owner, browser) = start_owner(&dirs, &[]);
    let browser = Proc::of(browser);
    let (_in_group, own_group, session) = helpers(&dirs.profile);
    owner.kill().unwrap(); // SIGKILL
    owner.wait().unwrap();
    assert!(
        browser.dies_within(DEATH_BOUND),
        "the browser outlived its owner by {DEATH_BOUND:?}"
    );
    assert!(
        own_group.alive() && session.alive(),
        "only our own child is signalled"
    );
}

/// `stop()`: SIGTERM then SIGKILL to the browser's group ends the browser and
/// every helper still in the group, and nothing outside it.
#[tokio::test]
async fn stop_ends_the_browser_group() {
    let dirs = dirs("helpers");
    let chrome = Chrome::launch(config(&dirs)).await.unwrap();
    let browser = Proc::of(chrome.pid() as i32);
    let (in_group, own_group, session) = helpers(&dirs.profile);
    let status = chrome.stop().await.unwrap();
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGTERM)
    );
    assert!(!browser.alive(), "stop returns after the browser exited");
    assert!(
        in_group.dies_within(DEATH_BOUND),
        "a helper in the group survived stop"
    );
    assert!(
        own_group.alive() && session.alive(),
        "only our own group is signalled"
    );
}

/// `stop()` sends SIGTERM to the browser alone, so Chrome shuts down in order;
/// only then does SIGKILL reach the rest of the group.
#[tokio::test]
async fn stop_sends_sigterm_to_the_browser_only() {
    let dirs = dirs("helpers");
    let chrome = Chrome::launch(config(&dirs)).await.unwrap();
    let (in_group, _own_group, _session) = helpers(&dirs.profile);
    chrome.stop().await.unwrap();
    assert!(in_group.dies_within(DEATH_BOUND));
    assert!(
        !dirs.profile.join("in-group-helper-sigterm").exists(),
        "stop sent SIGTERM to the whole group, not to the browser alone"
    );
}

/// A browser that does not exit on SIGTERM gets the full grace period (time
/// to flush cookies), then SIGKILL.
#[tokio::test]
async fn stop_gives_the_browser_five_seconds_before_sigkill() {
    let dirs = dirs("ignore-term");
    let chrome = Chrome::launch(config(&dirs)).await.unwrap();
    let started = Instant::now();
    let status = chrome.stop().await.unwrap();
    let took = started.elapsed();
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGKILL)
    );
    assert!(
        took >= Duration::from_millis(4900) && took < Duration::from_secs(8),
        "stop took {took:?}"
    );
}

/// An owner whose stdin closes (its test panicked, its host went away) exits,
/// and its browser goes with it.
#[test]
fn owner_exits_when_its_stdin_closes() {
    let dirs = dirs("serve");
    let (mut owner, browser) = start_owner(&dirs, &[]);
    let browser = Proc::of(browser);
    drop(owner.stdin.take());
    let deadline = Instant::now() + DEATH_BOUND;
    let mut exited = false;
    while !exited && Instant::now() < deadline {
        exited = owner.try_wait().unwrap().is_some();
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        owner.kill().unwrap();
        owner.wait().unwrap();
    }
    assert!(exited, "the owner kept running after its stdin closed");
    assert!(browser.dies_within(DEATH_BOUND));
}

/// PR_SET_PDEATHSIG fires when the forking thread exits, so the fork must not
/// happen on the caller's (possibly short-lived) thread.
#[test]
fn a_browser_outlives_the_thread_that_launched_it() {
    let dirs = dirs("serve");
    let config = config(&dirs);
    let (chrome, browser) = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let chrome = runtime.block_on(Chrome::launch(config)).unwrap();
        let browser = Proc::of(chrome.pid() as i32);
        (chrome, browser)
    })
    .join()
    .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        browser.alive(),
        "the browser died with the thread that launched it"
    );
    drop(chrome);
}

/// Descriptors the owner leaked without close-on-exec stay out of the
/// browser; the CDP pipes on 3 and 4 are there.
#[test]
fn owner_descriptors_do_not_reach_the_browser() {
    let dirs = dirs("serve");
    let (mut owner, browser) = start_owner(&dirs, &[]);
    let fds: BTreeMap<i32, PathBuf> = std::fs::read_dir(format!("/proc/{browser}/fd"))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let fd = entry.file_name().to_str()?.parse().ok()?;
            Some((fd, std::fs::read_link(entry.path()).ok()?))
        })
        .collect();
    owner.kill().unwrap();
    owner.wait().unwrap();
    let marker = dirs.profile.join("owner-inherited-marker");
    assert!(!fds.values().any(|target| *target == marker), "{fds:?}");
    for fd in [3, 4] {
        let target = fds.get(&fd).map(|t| t.to_string_lossy().into_owned());
        assert!(
            target.as_deref().is_some_and(|t| t.starts_with("pipe:")),
            "{fds:?}"
        );
    }
}

/// The profile and home are private (0700), also when they already exist
/// with a wider mode.
#[tokio::test]
async fn profile_and_home_are_made_private() {
    use std::os::unix::fs::PermissionsExt;
    let dirs = dirs("serve");
    std::fs::create_dir(&dirs.home).unwrap();
    for dir in [&dirs.profile, &dirs.home] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let chrome = Chrome::launch(config(&dirs)).await.unwrap();
    for dir in [&dirs.profile, &dirs.home] {
        let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
    chrome.stop().await.unwrap();
}

/// The owner's `stop` path (what `desktop` runs on stdin EOF) ends the browser.
#[test]
fn owner_stop_line_stops_the_browser() {
    let dirs = dirs("serve");
    let (mut owner, browser) = start_owner(&dirs, &[]);
    let browser = Proc::of(browser);
    owner.stdin.as_mut().unwrap().write_all(b"stop\n").unwrap();
    let mut out = String::new();
    BufReader::new(owner.stdout.as_mut().unwrap())
        .read_line(&mut out)
        .unwrap();
    owner.wait().unwrap();
    assert!(out.starts_with("stopped Ok("), "{out}");
    assert!(!browser.alive());
}

/// Restart overlap: a browser that hands its command line to a live profile
/// holder exits at once; launch reports that from the exit, not by waiting on
/// the pipe.
#[tokio::test]
async fn a_handoff_exit_is_profile_busy() {
    for code in [0, 21] {
        let dirs = dirs(&format!("exit:{code}"));
        let started = Instant::now();
        let result = Chrome::launch(config(&dirs)).await;
        assert!(started.elapsed() < DEATH_BOUND);
        match result {
            Err(Error::ProfileBusy { profile, status }) => {
                assert_eq!(profile, dirs.profile);
                assert_eq!(status.code(), Some(code));
            }
            other => panic!("expected ProfileBusy, got {:?}", other.map(|c| c.pid())),
        }
    }
}

#[tokio::test]
async fn another_early_exit_is_launch_exited() {
    let dirs = dirs("exit:3");
    match Chrome::launch(config(&dirs)).await {
        Err(Error::LaunchExited { status }) => assert_eq!(status.code(), Some(3)),
        other => panic!("expected LaunchExited, got {:?}", other.map(|c| c.pid())),
    }
}

#[tokio::test]
async fn a_missing_binary_is_a_spawn_error() {
    let dirs = dirs("serve");
    let mut config = config(&dirs);
    config.binary = dirs.home.join("no-such-chrome");
    match Chrome::launch(config).await {
        Err(Error::Spawn { binary, .. }) => assert!(binary.ends_with("no-such-chrome")),
        other => panic!("expected Spawn, got {:?}", other.map(|c| c.pid())),
    }
}
