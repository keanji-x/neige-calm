//! `stop` against real processes. Every process here is started by the test with a marker minted
//! from a fresh temporary `data_dir` and a fresh worker session id, so no sweep can match anything
//! the test did not create; every test kills what it started before asserting.

use std::io::Read as _;
use std::process::{Command, Stdio};

use super::{MARKER_KEY, MarkerInstance, Member, signal_verified, stop};
use crate::proc_identity::{parse_proc_stat_fields, read_proc_start_time};

struct Scope {
    _dir: tempfile::TempDir,
    instance: MarkerInstance,
    id: String,
}

impl Scope {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let instance = MarkerInstance::for_data_dir(dir.path()).expect("instance");
        Self {
            _dir: dir,
            instance,
            id: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn marker(&self) -> String {
        self.instance.marker(&self.id)
    }

    /// Live processes carrying this scope's exact marker, found without the code under test.
    fn marked_pids(&self) -> Vec<i32> {
        let needle = format!("{MARKER_KEY}={}", self.marker());
        std::fs::read_dir("/proc")
            .expect("proc")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
                    environ
                        .split(|&b| b == 0)
                        .any(|entry| entry == needle.as_bytes())
                })
            })
            .filter(|pid| alive(*pid))
            .collect()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        for pid in self.marked_pids() {
            kill(pid);
        }
    }
}

/// Run `script` under bash with exactly `PATH` and the given marker value, and return what it
/// printed; the script backgrounds its long-lived children with their output detached.
fn run_with_marker(marker: Option<&str>, script: &str) -> String {
    let mut command = Command::new("/bin/bash");
    command
        .args(["-c", script])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(marker) = marker {
        command.env(MARKER_KEY, marker);
    }
    let mut child = command.spawn().expect("spawn bash");
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read");
    child.wait().expect("wait bash");
    out
}

fn background_sleep(marker: Option<&str>) -> i32 {
    run_with_marker(marker, "sleep 300 </dev/null >/dev/null 2>&1 & echo $!")
        .trim()
        .parse()
        .expect("pid")
}

/// Dead means gone from `/proc` or a zombie awaiting its reaper.
fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_proc_stat_fields(&stat))
        .is_some_and(|fields| fields.state != 'Z')
}

fn kill(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

fn session_of(pid: i32) -> i32 {
    unsafe { libc::getsid(pid) }
}

#[tokio::test]
async fn stop_returns_ok_only_after_a_marked_setsid_child_is_gone() {
    let scope = Scope::new();
    run_with_marker(
        Some(&scope.marker()),
        "setsid sleep 300 </dev/null >/dev/null 2>&1 &",
    );
    let mut pids = Vec::new();
    for _ in 0..100 {
        pids = scope.marked_pids();
        if !pids.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(pids.len(), 1, "one marked setsid child: {pids:?}");
    let pid = pids[0];
    assert_ne!(
        session_of(pid),
        session_of(0),
        "the child runs in its own session, outside this process's group"
    );

    let result = stop(&scope.instance, &scope.id).await;
    let still_alive = alive(pid);
    kill(pid);

    assert!(result.is_ok(), "stop: {result:?}");
    assert!(
        !still_alive,
        "stop returned Ok while the marked child lived"
    );
}

#[tokio::test]
async fn stop_never_signals_a_readable_unmarked_or_near_miss_process() {
    let scope = Scope::new();
    let other = Scope::new();
    let unmarked = background_sleep(None);
    let longer = background_sleep(Some(&format!("{}x", scope.marker())));
    let other_instance = background_sleep(Some(&other.instance.marker(&scope.id)));
    let marked = background_sleep(Some(&scope.marker()));

    let result = stop(&scope.instance, &scope.id).await;
    let survivors = [unmarked, longer, other_instance].map(alive);
    let marked_alive = alive(marked);
    for pid in [unmarked, longer, other_instance, marked] {
        kill(pid);
    }

    assert!(result.is_ok(), "stop: {result:?}");
    assert_eq!(
        survivors,
        [true, true, true],
        "unmarked, near-miss and other-instance processes must never be signalled"
    );
    assert!(!marked_alive, "the marked process must be stopped");
}

/// A recycled pid presents as a live process whose `start_time` differs from the scanned one.
#[test]
fn a_member_whose_start_time_changed_is_never_signalled() {
    let scope = Scope::new();
    let pid = background_sleep(Some(&scope.marker()));
    let live = read_proc_start_time(pid).expect("start_time");

    let signalled = signal_verified(
        &[Member {
            pid,
            start_time: live + 1,
        }],
        libc::SIGKILL,
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let survived = alive(pid);
    kill(pid);

    assert!(signalled.is_empty(), "signalled {signalled:?}");
    assert!(
        survived,
        "a start_time-mismatched pid must never be signalled"
    );
}

#[test]
fn instances_differ_by_data_dir() {
    let one = Scope::new();
    let two = Scope::new();
    assert_ne!(one.instance, two.instance);
    assert_eq!(
        one.instance,
        MarkerInstance::for_data_dir(one._dir.path()).expect("instance")
    );
}
