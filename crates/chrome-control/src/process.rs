//! Ownership of the browser process.
//!
//! - **Fork.** Every browser is forked by one launcher thread that lives as
//!   long as the process. `PR_SET_PDEATHSIG` fires when the forking *thread*
//!   exits, so forking from a tokio blocking thread or any other short-lived
//!   thread would kill the browser when that thread retires.
//! - **Exit and reaping.** One waiter thread per browser blocks in
//!   `waitid(WEXITED | WNOWAIT)`, which reports the exit without reaping, and
//!   publishes the status. It reaps the zombie only after the owning
//!   [`ChildProcess`] is dropped. While the handle lives, the browser's pid,
//!   which is also its process-group id, cannot be reused, so signalling the
//!   group never reaches a process this crate did not start.
//!
//! Nothing else in the host process may reap this child (no `SIGCHLD` set to
//! `SIG_IGN`, no `waitpid(-1)`). tokio's own process reaper waits only for its
//! own children.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

use tokio::sync::{oneshot, watch};

use crate::{Error, Result};

type SpawnJob = (Command, oneshot::Sender<io::Result<Child>>);
type ExitSlot = Option<std::result::Result<ExitStatus, String>>;

/// The launcher's job queue. The static keeps one sender for the whole
/// process, so the launcher's receive loop never ends and the thread never exits.
static LAUNCHER: Mutex<Option<mpsc::Sender<SpawnJob>>> = Mutex::new(None);

fn launcher() -> io::Result<mpsc::Sender<SpawnJob>> {
    let mut slot = LAUNCHER.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(sender) = slot.as_ref() {
        return Ok(sender.clone());
    }
    let (sender, jobs) = mpsc::channel::<SpawnJob>();
    thread::Builder::new()
        .name("chrome-launcher".into())
        .spawn(move || {
            for (mut command, reply) in jobs {
                if let Err(Ok(mut child)) = reply.send(command.spawn()) {
                    // The launch was cancelled while forking, so nobody owns this
                    // child. It is unreaped, so its group id is still ours.
                    kill_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                }
            }
        })?;
    *slot = Some(sender.clone());
    Ok(sender)
}

/// A spawned browser process that this crate owns. Dropping it SIGKILLs the
/// browser's process group.
pub(crate) struct ChildProcess {
    pid: u32,
    exit: watch::Receiver<ExitSlot>,
    /// Set when waiting failed (something else reaped the child): from then on
    /// the pid may name another process, so the group is never signalled again.
    lost: Arc<AtomicBool>,
    /// Dropping this lets the waiter thread reap the zombie.
    _release: mpsc::Sender<()>,
}

impl ChildProcess {
    /// Spawns `command` on the launcher thread. The command must already put
    /// the child into its own process group.
    pub(crate) async fn spawn(command: Command) -> io::Result<Self> {
        let (reply, answer) = oneshot::channel();
        launcher()?
            .send((command, reply))
            .map_err(|_| io::Error::other("the Chrome launcher thread is gone"))?;
        let child = answer
            .await
            .map_err(|_| io::Error::other("the Chrome launcher thread dropped the job"))??;
        Self::own(child)
    }

    fn own(child: Child) -> io::Result<Self> {
        let pid = child.id();
        let (published, exit) = watch::channel(None);
        let (release, released) = mpsc::channel::<()>();
        let lost = Arc::new(AtomicBool::new(false));
        let lost_by_waiter = lost.clone();
        let waiter = thread::Builder::new()
            .name("chrome-waiter".into())
            .spawn(move || {
                let status = wait_without_reaping(pid);
                if status.is_err() {
                    lost_by_waiter.store(true, Ordering::SeqCst);
                }
                published.send_replace(Some(status.map_err(|error| error.to_string())));
                // Returns once the owning handle is dropped.
                let _ = released.recv();
                let mut child = child;
                let _ = child.wait();
            });
        if let Err(error) = waiter {
            // The child is still unreaped, so its group id is still ours.
            kill_group(pid, libc::SIGKILL);
            // SAFETY: plain waitpid on our own unreaped child.
            unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0) };
            return Err(error);
        }
        Ok(Self {
            pid,
            exit,
            lost,
            _release: release,
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Sends `signal` to the browser's process group. The leader is not reaped
    /// while `self` lives, so the group id still names the group we created.
    pub(crate) fn signal_group(&self, signal: libc::c_int) {
        if !self.lost.load(Ordering::SeqCst) {
            kill_group(self.pid, signal);
        }
    }

    /// Sends `signal` to the browser process alone. It is unreaped while
    /// `self` lives, so its pid still names it.
    pub(crate) fn signal_leader(&self, signal: libc::c_int) {
        if self.lost.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: kill has no memory effects. ESRCH cannot happen for an unreaped child.
        if unsafe { libc::kill(self.pid as libc::pid_t, signal) } != 0 {
            tracing::debug!(pid = self.pid, signal, error = %io::Error::last_os_error(), "kill");
        }
    }

    /// Resolves when the browser process has exited.
    pub(crate) async fn exited(&self) -> Result<ExitStatus> {
        let mut exit = self.exit.clone();
        let slot = exit
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Error::Wait("the waiter thread stopped".into()))?;
        match slot.as_ref() {
            Some(Ok(status)) => Ok(*status),
            Some(Err(error)) => Err(Error::Wait(error.clone())),
            None => Err(Error::Wait("no exit status".into())),
        }
    }

    /// A receiver that turns `Some` when the browser exits.
    pub(crate) fn exit_watch(&self) -> watch::Receiver<ExitSlot> {
        self.exit.clone()
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        // Runs before `_release` drops, so the leader is still unreaped here.
        self.signal_group(libc::SIGKILL);
    }
}

fn kill_group(pid: u32, signal: libc::c_int) {
    // SAFETY: killpg has no memory effects. ESRCH (group already empty) is fine.
    if unsafe { libc::killpg(pid as libc::pid_t, signal) } != 0 {
        tracing::debug!(pid, signal, error = %io::Error::last_os_error(), "killpg");
    }
}

/// Blocks until `pid` exits and returns its status, leaving it a zombie.
fn wait_without_reaping(pid: u32) -> io::Result<ExitStatus> {
    loop {
        // SAFETY: siginfo_t is plain data; waitid fills it on success.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOWAIT;
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
            // SAFETY: waitid succeeded for a child event, so si_status is valid.
            let status = unsafe { info.si_status() };
            // Re-encode as a wait(2) status word for ExitStatus.
            let raw = match info.si_code {
                libc::CLD_EXITED => (status & 0xff) << 8,
                libc::CLD_KILLED => status,
                libc::CLD_DUMPED => status | 0x80,
                // WEXITED reports only the three codes above.
                other => return Err(io::Error::other(format!("waitid si_code {other}"))),
            };
            return Ok(ExitStatus::from_raw(raw));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
