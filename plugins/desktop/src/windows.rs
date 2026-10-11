//! The open windows, as the compositor announces them, and waits on them.
//!
//! The compositor's `WindowEvent` stream is this table's only input: one
//! thread drains it (the compositor requires that) and bumps a change counter
//! that async waiters watch.
//!
//! A window belongs to the app launched as `pid` when its client is that
//! process or one of its descendants: the app launches Chrome's sandbox
//! (bwrap), and Chrome, the Wayland client, runs below it. Each window keeps
//! its client's lineage, read from `/proc` when the window opens, while the
//! client is connected, as (pid, start time) pairs: an ancestor that exited
//! since can have its pid reused by a later launch, which then has another
//! start time. The launched process is matched by its own pid and start time.

use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use compositor::{WindowEvent, WindowId, WindowInfo};
use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Clone)]
pub struct Windows {
    inner: Arc<Inner>,
}

struct Inner {
    open: Mutex<BTreeMap<WindowId, Entry>>,
    changes: watch::Sender<u64>,
}

struct Entry {
    info: WindowInfo,
    /// The client and its ancestors, nearest first.
    lineage: Vec<Proc>,
}

impl Entry {
    fn launched_by(&self, app: Option<Proc>) -> bool {
        app.is_some_and(|app| self.lineage.contains(&app))
    }
}

/// A process identity that survives pid reuse: its pid and its start time
/// (`/proc/<pid>/stat` field 22, clock ticks since boot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Proc {
    pid: u32,
    start: u64,
}

impl Proc {
    /// The process `pid` is now; `None` once it is gone (reaped).
    fn of(pid: u32) -> Option<Proc> {
        stat(pid).map(|(_, start)| Proc { pid, start })
    }
}

/// How many ancestors a lineage holds at most.
const MAX_LINEAGE: usize = 64;

impl Windows {
    /// Follows `events` on a thread of its own until the compositor stops.
    pub fn follow(events: Receiver<WindowEvent>) -> Self {
        let windows = Self {
            inner: Arc::new(Inner {
                open: Mutex::new(BTreeMap::new()),
                changes: watch::Sender::new(0),
            }),
        };
        let table = windows.clone();
        std::thread::Builder::new()
            .name("window-events".into())
            .spawn(move || {
                for event in events {
                    table.apply(event);
                }
            })
            .expect("spawn the window-events thread");
        windows
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<WindowId, Entry>> {
        self.inner
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The compositor announces a window `Opened` before any `Changed`.
    fn apply(&self, event: WindowEvent) {
        match event {
            WindowEvent::Opened(info) => {
                // Read from /proc before taking the table's lock.
                let lineage = lineage(info.pid);
                self.opened(info, lineage);
            }
            WindowEvent::Changed(info) => self.update(|open| {
                if let Some(entry) = open.get_mut(&info.id) {
                    entry.info = info;
                }
            }),
            WindowEvent::Closed(id) => self.update(|open| {
                open.remove(&id);
            }),
        }
    }

    fn opened(&self, info: WindowInfo, lineage: Vec<Proc>) {
        self.update(|open| {
            open.insert(info.id, Entry { info, lineage });
        });
    }

    fn update(&self, change: impl FnOnce(&mut BTreeMap<WindowId, Entry>)) {
        change(&mut self.lock());
        self.inner.changes.send_modify(|n| *n += 1);
    }

    /// Open windows, oldest first, each with whether the app launched as
    /// `app` (if any) owns it.
    pub fn list(&self, app: Option<u32>) -> Vec<(WindowInfo, bool)> {
        let app = app.and_then(Proc::of);
        self.lock()
            .values()
            .map(|e| (e.info.clone(), e.launched_by(app)))
            .collect()
    }

    pub fn get(&self, id: WindowId) -> Option<WindowInfo> {
        self.lock().get(&id).map(|e| e.info.clone())
    }

    /// The main window of the app launched as `pid`: its most recent open
    /// toplevel. Ids are assigned at map time, so the highest id is the newest.
    pub fn main_window(&self, pid: u32) -> Option<WindowInfo> {
        let app = Proc::of(pid);
        self.lock()
            .values()
            .rev()
            .find(|e| e.launched_by(app))
            .map(|e| e.info.clone())
    }

    /// A receiver that sees every change to the table.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Waits up to `timeout` for `pid`'s main window, giving up early when
    /// `still_wanted` turns false (the app exited or was relaunched).
    pub async fn wait_main_window(
        &self,
        pid: u32,
        timeout: Duration,
        still_wanted: impl Fn() -> bool,
    ) -> Option<WindowInfo> {
        let deadline = Instant::now() + timeout;
        let mut changes = self.inner.changes.subscribe();
        loop {
            changes.mark_unchanged();
            if let Some(window) = self.main_window(pid) {
                return Some(window);
            }
            if !still_wanted() {
                return None;
            }
            // Re-check `still_wanted` at least twice a second.
            let tick = (Instant::now() + Duration::from_millis(500)).min(deadline);
            match tokio::time::timeout_at(tick, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return None,
                Err(_) if Instant::now() >= deadline => return None,
                Err(_) => {}
            }
        }
    }
}

/// `pid` and its ancestors, nearest first, up to but not including init.
/// The walk stops at a "parent" that started after its child: the real
/// parent exited between the two reads and its pid was reused.
fn lineage(pid: i32) -> Vec<Proc> {
    lineage_from(pid, stat)
}

fn lineage_from(pid: i32, stat: impl Fn(u32) -> Option<(u32, u64)>) -> Vec<Proc> {
    let mut chain: Vec<Proc> = Vec::new();
    let mut next = u32::try_from(pid).ok();
    while let Some(pid) = next.filter(|&p| p > 1 && chain.len() < MAX_LINEAGE) {
        let Some((parent, start)) = stat(pid) else {
            break;
        };
        if chain.last().is_some_and(|child| start > child.start) {
            break;
        }
        chain.push(Proc { pid, start });
        next = Some(parent);
    }
    chain
}

/// (parent pid, start time) from `/proc/<pid>/stat`; `None` once the process
/// is gone.
fn stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may hold spaces and parentheses: fields follow the last
    // `)`, starting with field 3 (state); ppid is field 4, starttime field 22.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u64, pid: i32) -> WindowInfo {
        WindowInfo {
            id: WindowId(id),
            title: format!("w{id}"),
            size: (10, 10),
            pid,
        }
    }

    /// Two live processes this test owns, each a client of its own windows.
    fn sleepers() -> (std::process::Child, std::process::Child) {
        let spawn = || {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        };
        (spawn(), spawn())
    }

    #[tokio::test]
    async fn the_main_window_is_the_newest_open_window_of_the_pid() {
        let (mut a, mut b) = sleepers();
        let (pa, pb) = (a.id(), b.id());
        let (tx, rx) = std::sync::mpsc::channel();
        let windows = Windows::follow(rx);
        let waiter = {
            let windows = windows.clone();
            tokio::spawn(async move {
                windows
                    .wait_main_window(pa, Duration::from_secs(5), || true)
                    .await
            })
        };
        tx.send(WindowEvent::Opened(info(1, pb as i32))).unwrap();
        tx.send(WindowEvent::Opened(info(2, pa as i32))).unwrap();
        assert_eq!(waiter.await.unwrap().unwrap().id, WindowId(2));
        tx.send(WindowEvent::Opened(info(3, pa as i32))).unwrap();
        tx.send(WindowEvent::Opened(info(4, pb as i32))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(windows.main_window(pb).unwrap().id, WindowId(4));
        assert_eq!(windows.main_window(pa).unwrap().id, WindowId(3));
        tx.send(WindowEvent::Closed(WindowId(3))).unwrap();
        tx.send(WindowEvent::Closed(WindowId(4))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(windows.main_window(pa).unwrap().id, WindowId(2));
        assert_eq!(windows.main_window(pb).unwrap().id, WindowId(1));
        for child in [&mut a, &mut b] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        // A reaped launch owns nothing any more.
        assert!(windows.main_window(pa).is_none());
    }

    /// A window whose client runs below the launched process (Chrome inside
    /// its sandbox) is that app's; another process's window is not.
    #[tokio::test]
    async fn a_window_belongs_to_the_app_that_launched_its_client() {
        let (tx, rx) = std::sync::mpsc::channel();
        let windows = Windows::follow(rx);
        let client = std::process::id();
        // SAFETY: getppid has no preconditions.
        let launcher = unsafe { libc::getppid() } as u32;
        let chain: Vec<u32> = lineage(client as i32).iter().map(|p| p.pid).collect();
        assert!(chain.starts_with(&[client, launcher]), "{chain:?}");
        // Not below this process: kthreadd, or whatever pid 2 is here.
        let other = 2;
        assert!(!lineage(other).iter().any(|p| p.pid == client));
        tx.send(WindowEvent::Opened(info(1, client as i32)))
            .unwrap();
        tx.send(WindowEvent::Opened(info(2, other))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(windows.main_window(launcher).unwrap().id, WindowId(1));
        assert_eq!(windows.main_window(client).unwrap().id, WindowId(1));
        let owned: Vec<bool> = windows
            .list(Some(launcher))
            .into_iter()
            .map(|(_, o)| o)
            .collect();
        assert_eq!(owned, [true, false]);
        assert!(windows.list(None).iter().all(|(_, owned)| !owned));
    }

    /// `start` is the start time (field 22), not the parent pid or the thread
    /// count: two children of one parent, started apart, differ in it.
    #[test]
    fn a_process_identity_carries_its_start_time() {
        let spawn = || {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        };
        let mut first = spawn();
        std::thread::sleep(Duration::from_millis(50));
        let mut second = spawn();
        let (a, b) = (Proc::of(first.id()), Proc::of(second.id()));
        let parent = stat(first.id()).map(|(ppid, _)| ppid);
        for child in [&mut first, &mut second] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(a.start > 0 && a.start < b.start, "{a:?} {b:?}");
        assert_eq!(parent, Some(std::process::id()));
        // Neither the parent pid nor the thread count (one for `sleep`).
        assert!(
            a.start != u64::from(std::process::id()) && a.start != 1,
            "{a:?}"
        );
    }

    /// A parent pid read after the real parent exited can name a newer
    /// process: the walk stops there instead of recording it.
    #[test]
    fn the_walk_stops_at_a_parent_younger_than_its_child() {
        // pid -> (ppid, start): 30's real parent 20 exited, and 20 now names
        // a process started after 30.
        let table = |pid: u32| match pid {
            30 => Some((20, 500)),
            20 => Some((10, 900)),
            10 => Some((1, 100)),
            _ => None,
        };
        assert_eq!(
            lineage_from(30, table),
            [Proc {
                pid: 30,
                start: 500
            }]
        );
        let honest = |pid: u32| match pid {
            30 => Some((20, 500)),
            20 => Some((1, 400)),
            _ => None,
        };
        assert_eq!(
            lineage_from(30, honest),
            [
                Proc {
                    pid: 30,
                    start: 500
                },
                Proc {
                    pid: 20,
                    start: 400
                }
            ]
        );
    }

    /// A window whose recorded ancestor has exited is not the app's when a
    /// new launch reuses that ancestor's pid: the start times differ.
    #[tokio::test]
    async fn a_reused_ancestor_pid_does_not_claim_a_stranger_window() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let windows = Windows::follow(rx);
        // The new launch: this test process, alive under its pid now.
        let launch = Proc::of(std::process::id()).unwrap();
        // The stranger's lineage recorded an earlier process under that pid.
        let stranger = vec![
            Proc {
                pid: 999_999_999,
                start: 1,
            },
            Proc {
                pid: launch.pid,
                start: launch.start - 1,
            },
        ];
        windows.opened(info(1, 999_999_999), stranger);
        assert!(windows.main_window(launch.pid).is_none());
        assert!(!windows.list(Some(launch.pid))[0].1);
        // The same lineage with the launch's own start time is the app's.
        windows.opened(info(2, 999_999_999), vec![launch]);
        assert_eq!(windows.main_window(launch.pid).unwrap().id, WindowId(2));
    }

    #[tokio::test]
    async fn the_wait_ends_at_its_timeout_or_when_unwanted() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let windows = Windows::follow(rx);
        let started = std::time::Instant::now();
        assert!(
            windows
                .wait_main_window(7, Duration::from_millis(100), || true)
                .await
                .is_none()
        );
        assert!(started.elapsed() >= Duration::from_millis(100));
        let started = std::time::Instant::now();
        assert!(
            windows
                .wait_main_window(7, Duration::from_secs(30), || false)
                .await
                .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
