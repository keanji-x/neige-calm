//! `stop` against real processes. Every process here is started by the test with a marker minted
//! from a fresh temporary `data_dir` and a fresh worker session id, so no sweep can match anything
//! the test did not create. Cleanup never kills a bare pid: every process is captured with its
//! `start_time` when it is created and killed only through [`reap`], the `start_time`-verified
//! signal, so a pid recycled after `stop` (these sleeps are reaped by init, not by the test) is
//! never hit.

use std::collections::HashSet;
use std::io::Read as _;
use std::process::{Command, Stdio};

use super::{MARKER_KEY, MarkerInstance, Member, scan_in, signal_verified, stop};
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

    /// Live processes carrying this scope's exact marker, found without the code under test; the
    /// `start_time` is read before the environ, as the sweep does.
    fn marked(&self) -> Vec<Member> {
        let needle = format!("{MARKER_KEY}={}", self.marker());
        std::fs::read_dir("/proc")
            .expect("proc")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| {
                Some(Member {
                    pid,
                    start_time: read_proc_start_time(pid)?,
                })
            })
            .filter(|member| {
                std::fs::read(format!("/proc/{}/environ", member.pid)).is_ok_and(|environ| {
                    environ
                        .split(|&b| b == 0)
                        .any(|entry| entry == needle.as_bytes())
                })
            })
            .filter(|member| alive(member.pid))
            .collect()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        for member in self.marked() {
            reap(member);
        }
    }
}

/// The only way a test kills: the `start_time`-verified signal.
fn reap(member: Member) {
    signal_verified(&[member], libc::SIGKILL);
}

/// Reaps (verified) the processes a test started without this scope's marker, even when an
/// assertion fails first.
struct Started(Vec<Member>);

impl Drop for Started {
    fn drop(&mut self) {
        for member in &self.0 {
            reap(*member);
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

/// A backgrounded `sleep 300`, captured with its `start_time` while it is certainly alive.
fn background_sleep(marker: Option<&str>) -> Member {
    let pid = run_with_marker(marker, "sleep 300 </dev/null >/dev/null 2>&1 & echo $!")
        .trim()
        .parse()
        .expect("pid");
    let start_time = read_proc_start_time(pid).expect("a fresh sleep has a start_time");
    Member { pid, start_time }
}

/// Dead means gone from `/proc` or a zombie awaiting its reaper.
fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_proc_stat_fields(&stat))
        .is_some_and(|fields| fields.state != 'Z')
}

/// Alive AND still the process that was captured.
fn still(member: Member) -> bool {
    alive(member.pid) && read_proc_start_time(member.pid) == Some(member.start_time)
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
    let mut members = Vec::new();
    for _ in 0..100 {
        members = scope.marked();
        if !members.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(members.len(), 1, "one marked setsid child: {members:?}");
    let child = members[0];
    assert_ne!(
        session_of(child.pid),
        session_of(0),
        "the child runs in its own session, outside this process's group"
    );

    let result = stop(&scope.instance, &scope.id).await;
    let still_alive = still(child);
    reap(child);

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
    let _started = Started(vec![unmarked, longer, other_instance, marked]);

    let result = stop(&scope.instance, &scope.id).await;
    let survivors = [unmarked, longer, other_instance].map(still);
    let marked_alive = still(marked);

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
    let sleep = background_sleep(Some(&scope.marker()));
    let recycled = Member {
        start_time: sleep.start_time + 1,
        ..sleep
    };

    let signalled = signal_verified(&[recycled], libc::SIGKILL);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let survived = still(sleep);
    reap(sleep);

    assert!(signalled.is_empty(), "signalled {signalled:?}");
    assert!(
        survived,
        "a start_time-mismatched pid must never be signalled"
    );
}

/// The test cleanup path itself: a captured pid that now names another process is left alone.
#[test]
fn test_cleanup_never_kills_a_recycled_pid() {
    let unrelated = background_sleep(None);
    let _started = Started(vec![unrelated]);
    reap(Member {
        start_time: unrelated.start_time + 1,
        ..unrelated
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let survived = still(unrelated);

    assert!(
        survived,
        "cleanup killed a process that was not the one it captured"
    );
}

#[test]
fn an_unlistable_proc_root_is_an_error_not_an_empty_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let markers = HashSet::from(["x:y".to_string()]);
    let missing = scan_in(&dir.path().join("no-such-proc"), &markers);
    assert!(missing.is_err(), "{missing:?}");
    assert_eq!(
        scan_in(dir.path(), &markers).expect("empty root"),
        Vec::new()
    );
}

/// A fake `/proc` with one pid directory whose `stat` and `environ` the test writes.
fn fake_proc(pid: i32, stat: Option<&str>, environ: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(pid.to_string());
    std::fs::create_dir(&dir).expect("pid dir");
    if let Some(stat) = stat {
        std::fs::write(dir.join("stat"), stat).expect("stat");
    }
    std::fs::write(dir.join("environ"), environ).expect("environ");
    root
}

const GOOD_STAT: &str = "4242 (sleep) S 1 4242 4242 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 7777 0 0";

#[test]
fn a_fake_proc_root_pins_the_per_pid_rules() {
    let markers = HashSet::from(["inst:ws".to_string()]);
    let marked = "PATH=/bin\0NEIGE_CLAUDE_PLANNER=inst:ws\0";
    let foreign = "PATH=/bin\0NEIGE_CLAUDE_PLANNER=inst:other\0";

    let root = fake_proc(4242, Some(GOOD_STAT), marked);
    assert_eq!(
        scan_in(root.path(), &markers).expect("scan"),
        vec![Member {
            pid: 4242,
            start_time: 7777
        }],
        "a marked pid with a readable stat is a member"
    );

    let root = fake_proc(4242, Some("garbage"), marked);
    assert!(
        scan_in(root.path(), &markers).is_err(),
        "a marked pid whose stat is unreadable cannot be signalled or proven gone"
    );

    let root = fake_proc(4242, Some("garbage"), foreign);
    assert_eq!(
        scan_in(root.path(), &markers).expect("scan"),
        Vec::new(),
        "an unmarked pid with a garbage stat is not this sweep's business"
    );

    let root = fake_proc(4242, None, marked);
    assert_eq!(
        scan_in(root.path(), &markers).expect("scan"),
        Vec::new(),
        "a pid whose stat vanished is skipped"
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
