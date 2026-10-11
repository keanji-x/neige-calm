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
//! client is connected. A launched process stays unreaped while its app runs,
//! so its pid names no other process meanwhile.

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
    /// The client's pid and its ancestors' pids, nearest first.
    lineage: Vec<u32>,
}

impl Entry {
    fn launched_by(&self, pid: u32) -> bool {
        self.lineage.contains(&pid)
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

    fn apply(&self, event: WindowEvent) {
        {
            let mut open = self.lock();
            match event {
                WindowEvent::Opened(info) | WindowEvent::Changed(info) => {
                    match open.get_mut(&info.id) {
                        Some(entry) => entry.info = info,
                        None => {
                            let lineage = lineage(info.pid);
                            open.insert(info.id, Entry { info, lineage });
                        }
                    }
                }
                WindowEvent::Closed(id) => {
                    open.remove(&id);
                }
            }
        }
        self.inner.changes.send_modify(|n| *n += 1);
    }

    /// Open windows, oldest first, each with whether the app launched as
    /// `app` (if any) owns it.
    pub fn list(&self, app: Option<u32>) -> Vec<(WindowInfo, bool)> {
        self.lock()
            .values()
            .map(|e| (e.info.clone(), app.is_some_and(|pid| e.launched_by(pid))))
            .collect()
    }

    pub fn get(&self, id: WindowId) -> Option<WindowInfo> {
        self.lock().get(&id).map(|e| e.info.clone())
    }

    /// The main window of the app launched as `pid`: its most recent open
    /// toplevel. Ids are assigned at map time, so the highest id is the newest.
    pub fn main_window(&self, pid: u32) -> Option<WindowInfo> {
        self.lock()
            .values()
            .rev()
            .find(|e| e.launched_by(pid))
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
fn lineage(pid: i32) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut next = u32::try_from(pid).ok();
    while let Some(pid) = next.filter(|&p| p > 1 && chain.len() < MAX_LINEAGE) {
        chain.push(pid);
        next = parent(pid);
    }
    chain
}

/// The parent pid from `/proc/<pid>/stat`; `None` once the process is gone.
fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may hold spaces and parentheses: fields follow the last `)`.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
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

    #[tokio::test]
    async fn the_main_window_is_the_newest_open_window_of_the_pid() {
        let (tx, rx) = std::sync::mpsc::channel();
        let windows = Windows::follow(rx);
        let waiter = {
            let windows = windows.clone();
            tokio::spawn(async move {
                windows
                    .wait_main_window(7, Duration::from_secs(5), || true)
                    .await
            })
        };
        tx.send(WindowEvent::Opened(info(1, 9))).unwrap();
        tx.send(WindowEvent::Opened(info(2, 7))).unwrap();
        assert_eq!(waiter.await.unwrap().unwrap().id, WindowId(2));
        tx.send(WindowEvent::Opened(info(3, 7))).unwrap();
        tx.send(WindowEvent::Opened(info(4, 9))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(windows.main_window(9).unwrap().id, WindowId(4));
        assert_eq!(windows.main_window(7).unwrap().id, WindowId(3));
        tx.send(WindowEvent::Closed(WindowId(3))).unwrap();
        tx.send(WindowEvent::Closed(WindowId(4))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(windows.main_window(7).unwrap().id, WindowId(2));
        assert_eq!(windows.main_window(9).unwrap().id, WindowId(1));
        assert!(windows.main_window(8).is_none());
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
        assert!(lineage(client as i32).starts_with(&[client, launcher]));
        // Not below this process: kthreadd, or whatever pid 2 is here.
        let other = 2;
        assert!(!lineage(other).contains(&client));
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
