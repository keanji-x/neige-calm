//! The real `desktop` binary always runs its browser (the fake Chrome) inside
//! the bwrap sandbox, and refuses to run it any other way.
#![cfg(target_os = "linux")]

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use support::*;

/// The fence: the browser sees its fixed paths and the `/etc` allowlist, and
/// none of the host's files, the plugin's own data directory included.
#[test]
fn the_browser_sees_only_its_sandbox() {
    let tmp = tmp();
    let data = tmp.path().join("desktop");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, b"not for Chrome").unwrap();
    let mut plugin = Plugin::spawn(&data);
    assert!(plugin.initialize(FAKE_CHROME).get("result").is_some());

    let mut expected: BTreeMap<String, &str> = BTreeMap::new();
    let mut hidden = |path: &Path| {
        if path.exists() {
            expected.insert(path.to_str().unwrap().to_owned(), "absent");
        }
    };
    hidden(&secret);
    hidden(&data);
    hidden(&data.join("home"));
    hidden(Path::new(FAKE_CHROME));
    if let Some(home) = std::env::var_os("HOME") {
        hidden(Path::new(&home));
    }
    hidden(Path::new("/mnt/data2"));
    hidden(Path::new("/home"));
    hidden(Path::new("/etc/passwd"));
    hidden(Path::new("/etc/shadow"));
    hidden(Path::new("/etc/ssh"));
    for path in [
        "/data/home/probe-paths",
        "/data/profile",
        "/opt/chrome/desktop-fake-chrome",
        "/run/wayland/wayland-0",
        "/etc/hosts",
        "/usr/bin",
    ] {
        expected.insert(path.to_owned(), "present");
    }
    let list: String = expected.keys().map(|p| format!("{p}\n")).collect();
    std::fs::write(plugin.home().join("probe-paths"), list).unwrap();

    std::fs::write(plugin.home().join("hold-window"), b"").unwrap();
    let fake = launch_fake(&mut plugin);
    let seen: BTreeMap<String, &str> = std::fs::read_to_string(plugin.home().join("probe-result"))
        .unwrap()
        .lines()
        .map(|line| {
            let (path, state) = line.rsplit_once(' ').unwrap();
            let state = if state == "present" {
                "present"
            } else {
                "absent"
            };
            (path.to_owned(), state)
        })
        .collect();
    assert_eq!(seen, expected);

    // Its own pid namespace, and the environment of its fixed paths.
    assert_ne!(plugin.fake_inner_pid(), fake);
    let env = environ(fake);
    assert_eq!(env.get("HOME").map(String::as_str), Some("/data/home"));
    assert_eq!(
        env.get("WAYLAND_DISPLAY").map(String::as_str),
        Some("wayland-0")
    );
    assert_eq!(
        env.get("XDG_RUNTIME_DIR").map(String::as_str),
        Some("/run/wayland")
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    // PID/CDP readiness does not imply that Wayland has published a window.
    let held = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
    assert!(held["windows"].as_array().unwrap().is_empty(), "{held}");
    std::fs::remove_file(plugin.home().join("hold-window")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let ls = loop {
        let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
        if !ls["windows"].as_array().unwrap().is_empty() {
            break ls;
        }
        assert!(Instant::now() < deadline, "no window within 5s: {ls}");
        std::thread::sleep(Duration::from_millis(20));
    };
    // Its window is the app's, although bwrap is the launched process.
    assert_eq!(ls["windows"][0]["app"], "chrome", "{ls}");
}

/// Without bwrap the plugin does not start, so nothing can launch Chrome
/// unsandboxed.
#[test]
fn without_bwrap_the_plugin_does_not_start() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    let missing = tmp.path().join("no-such-bwrap");
    let reply = plugin.initialize_with(FAKE_CHROME, missing.to_str().unwrap());
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("bwrap_binary"), "{reply}");
    let status = plugin.wait_exit(Duration::from_secs(5)).expect("exits");
    assert!(!status.success(), "{status}");
}

/// A Chrome install whose directory holds the plugin's data dir fails the
/// plugin start: the sandbox would show that directory to Chrome.
#[test]
fn an_install_directory_holding_the_data_dir_fails_initialize() {
    let tmp = tmp();
    let chrome = tmp.path().join("chrome");
    std::fs::write(&chrome, b"").unwrap();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    let reply = plugin.initialize(chrome.to_str().unwrap());
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("directory of its own"), "{reply}");
    let status = plugin.wait_exit(Duration::from_secs(5)).expect("exits");
    assert!(!status.success(), "{status}");
}

/// A sandbox that cannot start is an error for the caller; the browser never
/// runs in its place.
#[test]
fn a_sandbox_that_cannot_start_runs_no_browser() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    // Executable, so the plugin starts, but it never runs the browser.
    let reply = plugin.initialize_with(FAKE_CHROME, "/usr/bin/false");
    assert!(reply.get("result").is_some(), "{reply}");
    let err = plugin
        .tool("page_open", json!({ "url": "https://example.com/" }))
        .unwrap_err();
    assert_eq!(err["code"], -32503, "{err}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("exited during launch"),
        "{err}"
    );
    assert!(!plugin.home().join("fake-chrome.pid").exists());
    let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
    assert_eq!(ls["apps"][0]["running"], false, "{ls}");
}

/// A SIGKILLed desktop takes the whole sandbox with it: bwrap, its init and
/// the browser.
#[test]
fn every_sandbox_process_ends_with_a_killed_desktop() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    plugin.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut plugin);
    let inside: Vec<(u32, u64)> = descendants(plugin.pid())
        .into_iter()
        .filter_map(|pid| Some((pid, start_time(pid)?)))
        .collect();
    assert!(inside.iter().any(|&(pid, _)| pid == fake));
    assert!(
        inside.len() >= 3,
        "bwrap, its init and the browser: {inside:?}"
    );
    // The test's own child.
    unsafe { libc::kill(plugin.pid() as i32, libc::SIGKILL) };
    plugin.wait_exit(Duration::from_secs(5)).expect("killed");
    wait_for(Duration::from_secs(5), || {
        inside
            .iter()
            .all(|&(pid, start)| start_time(pid) != Some(start))
    });
}

/// Chrome's profile lock does not hold across sandboxes, so `http.sock` is
/// the gate: a desktop stopping in order keeps answering on it until its
/// Chrome is gone, and a new desktop on the same data dir neither binds nor
/// launches before then. The fake does not close on `Browser.close`, so the
/// old stop takes its full close grace (5 s).
#[test]
fn a_new_desktop_waits_until_the_stopping_one_has_ended_its_chrome() {
    let tmp = tmp();
    let data = tmp.path().join("desktop");
    let mut old = Plugin::spawn(&data);
    old.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut old);
    let stopping = std::time::Instant::now();
    old.close_stdin();
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(fake), "the old Chrome is still stopping");

    let mut new = Plugin::spawn(&data);
    let reply = new.initialize(FAKE_CHROME);
    let waited = stopping.elapsed();
    assert!(reply.get("result").is_some(), "{reply}");
    assert!(!alive(fake), "a new desktop started beside a live Chrome");
    assert!(waited >= Duration::from_secs(4), "{waited:?}");
    assert!(old.wait_exit(Duration::from_secs(5)).is_some());
}

/// The start time of a live, non-zombie `pid`.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    (fields[0] != "Z").then(|| fields[19].parse().ok())?
}

fn environ(pid: u32) -> BTreeMap<String, String> {
    std::fs::read(format!("/proc/{pid}/environ"))
        .unwrap()
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let (key, value) = std::str::from_utf8(entry).ok()?.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}
