//! Runs only in the owned helper process, never in the ProcRegistry host.
//! All adopted children are collected by their reported PID, never by wildcard wait.

use calm_session::control::execution::ExecutionSpecification;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use workspace::execution::{CapabilityToken, ExecutionId, ExecutorBootId, ScopeMechanismIdentity};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct HelperContext {
    pub execution_id: ExecutionId,
    pub executor_boot: ExecutorBootId,
    pub nonce: CapabilityToken,
    pub mechanism: ScopeMechanismIdentity,
}
#[derive(Serialize, Deserialize)]
pub(super) struct HelperPlan {
    pub context: HelperContext,
    pub specification: ExecutionSpecification,
}
#[derive(Serialize, Deserialize)]
pub(super) struct HelperReport {
    pub context: HelperContext,
    pub event: HelperEvent,
}

use std::{
    fs::File,
    io::{BufRead, Write},
    os::fd::{FromRawFd, RawFd},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::atomic::{AtomicI32, Ordering},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum HelperEvent {
    Started,
    LeaderExited { status: Option<i32> },
    Drained { status: Option<i32> },
    Unknown { reason: String },
}

static REQUESTED_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn remember_signal(signal: i32) {
    REQUESTED_SIGNAL.store(signal, Ordering::SeqCst);
}

pub fn run(config_fd: RawFd, report_fd: RawFd) -> anyhow::Result<()> {
    anyhow::ensure!(
        config_fd > 2 && report_fd > 2 && config_fd != report_fd,
        "invalid helper descriptors"
    );
    // SAFETY: only the execution-helper entry point transfers these inherited descriptors.
    let config = unsafe { File::from_raw_fd(config_fd) };
    // SAFETY: distinct inherited descriptor owned by this helper only.
    let mut report = unsafe { File::from_raw_fd(report_fd) };
    // A parent's ignored SIGCHLD can survive exec; this helper must retain its own child zombies.
    // SAFETY: this is the dedicated helper before its single target fork, not the supervisor host.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        anyhow::ensure!(
            libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) == 0,
            "helper SIGCHLD setup failed"
        );
    }
    // SAFETY: this process is the dedicated helper, not the supervisor host.
    anyhow::ensure!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } == 0,
        "helper subreaper setup failed"
    );
    // Prevent the untrusted child from obtaining its parent's private report descriptor.
    // SAFETY: applies only to this helper and does not alter the target child's eventual exec.
    anyhow::ensure!(
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } == 0,
        "helper process protection failed"
    );
    super::process::cloexec(report_fd, true)?;
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGUSR1] {
        // SAFETY: handler performs only an atomic store; no allocation or registry access.
        unsafe {
            libc::signal(signal, remember_signal as *const () as libc::sighandler_t);
        }
    }
    let plan: HelperPlan = serde_json::from_reader(config)?;
    let image = std::fs::metadata("/proc/self/exe")?;
    anyhow::ensure!(
        plan.context.mechanism.executable_device == image.dev()
            && plan.context.mechanism.executable_inode == image.ino()
            && plan.context.mechanism.protocol_version == 1,
        "unregistered helper image/protocol"
    );
    let specification = plan.specification;
    let context = plan.context;
    let mut command = Command::new(&specification.program);
    command
        .args(&specification.args)
        .env_clear()
        .envs(specification.environment.iter().cloned())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // SAFETY: the child changes only its own exec-surviving descriptor set.
    unsafe {
        command.pre_exec(super::process::mark_child_descriptors_cloexec);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send(
                &mut report,
                &context,
                &HelperEvent::Unknown {
                    reason: format!("target executable did not start: {error}"),
                },
            );
            if matches!(peek_child()?, ChildObservation::NoChildren) {
                send(
                    &mut report,
                    &context,
                    &HelperEvent::Drained { status: Some(127) },
                );
            }
            return Ok(());
        }
    };
    send(&mut report, &context, &HelperEvent::Started);
    let mut status = None;
    let mut root_exited = false;
    loop {
        if !root_exited {
            if let Some(exit) = child.try_wait()? {
                status = exit.code();
                root_exited = true;
                send(&mut report, &context, &HelperEvent::LeaderExited { status });
            }
        }
        // This kernel-reported list belongs to this dedicated single owner, not the host.
        let children = immediate_children()?;
        let signal = REQUESTED_SIGNAL.load(Ordering::SeqCst);
        for pid in &children {
            if signal != 0 {
                signal_child(
                    *pid,
                    if signal == libc::SIGUSR1 {
                        libc::SIGKILL
                    } else {
                        signal
                    },
                )?;
            }
        }
        match peek_child()? {
            ChildObservation::NoChildren if root_exited => {
                // No further fork entry exists here. ECHILD is kernel evidence, not a /proc snapshot.
                send(&mut report, &context, &HelperEvent::Drained { status });
                return Ok(());
            }
            ChildObservation::NoChildren => {
                anyhow::bail!("helper lost the unreaped target before exit observation")
            }
            ChildObservation::Exited(pid) if pid != child.id() || root_exited => {
                let mut ignored_status = 0;
                // SAFETY: WNOWAIT retained this exact child zombie; this helper alone reaps it.
                if unsafe { libc::waitpid(pid as i32, &mut ignored_status, libc::WNOHANG) } < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            ChildObservation::Exited(_) | ChildObservation::LiveChildren => {}
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

enum ChildObservation {
    NoChildren,
    LiveChildren,
    Exited(u32),
}

fn peek_child() -> anyhow::Result<ChildObservation> {
    // This non-consuming observation is confined to the owned helper, which never hosts ProcRegistry.
    // SAFETY: valid siginfo output; WNOWAIT explicitly forbids consuming any child's zombie.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_ALL,
            0,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ECHILD) => Ok(ChildObservation::NoChildren),
            Some(libc::EINTR) => Ok(ChildObservation::LiveChildren),
            _ => Err(std::io::Error::last_os_error().into()),
        };
    }
    // SAFETY: waitid initializes the SIGCHLD union; zero pid means children exist but none exited.
    let pid = unsafe { info.si_pid() };
    Ok(if pid == 0 {
        ChildObservation::LiveChildren
    } else {
        ChildObservation::Exited(u32::try_from(pid)?)
    })
}

fn send(report: &mut File, context: &HelperContext, event: &HelperEvent) {
    let message = HelperReport {
        context: context.clone(),
        event: event.clone(),
    };
    if serde_json::to_writer(&mut *report, &message).is_ok() {
        let _ = report.write_all(b"\n");
        let _ = report.flush();
    }
}

fn immediate_children() -> anyhow::Result<Vec<u32>> {
    let path = format!("/proc/self/task/{}/children", std::process::id());
    let contents = std::fs::read_to_string(path)?;
    contents
        .split_whitespace()
        .map(|pid| pid.parse().map_err(Into::into))
        .collect()
}

fn signal_child(pid: u32, signal: i32) -> anyhow::Result<()> {
    // Child PIDs cannot be reused before this sole helper has reaped them.
    // SAFETY: positive immediate child PID, never a process-group wildcard.
    let result = unsafe { libc::kill(i32::try_from(pid)?, signal) };
    if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub(super) fn reports(file: File, send: tokio::sync::mpsc::UnboundedSender<HelperReport>) {
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            break;
        };
        if line.len() > 64 * 1024 {
            break;
        }
        let Ok(event) = serde_json::from_str(&line) else {
            break;
        };
        if send.send(event).is_err() {
            break;
        }
    }
}
