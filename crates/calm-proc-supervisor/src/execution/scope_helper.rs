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
pub(super) enum HelperPlan {
    Execution {
        context: HelperContext,
        specification: ExecutionSpecification,
        #[serde(
            default,
            skip_serializing_if = "crate::child_environment::LaunchEnvironment::is_empty"
        )]
        environment: crate::child_environment::LaunchEnvironment,
    },
}
#[derive(Serialize, Deserialize)]
pub(super) struct HelperReport {
    pub context: HelperContext,
    pub event: HelperEvent,
}

use std::{
    fs::File,
    io::{BufRead, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::atomic::{AtomicI32, Ordering},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum HelperEvent {
    Ready,
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
    run_with_command_hook(config_fd, report_fd, |_| {})
}

pub(super) fn run_with_command_hook(
    config_fd: RawFd,
    report_fd: RawFd,
    before_spawn: impl FnOnce(&mut Command),
) -> anyhow::Result<()> {
    run_with_hooks(config_fd, report_fd, before_spawn, || {})
}

pub(super) fn run_with_hooks(
    config_fd: RawFd,
    report_fd: RawFd,
    before_spawn: impl FnOnce(&mut Command),
    after_spawn: impl FnOnce(),
) -> anyhow::Result<()> {
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
    let mechanism = match &plan {
        HelperPlan::Execution { context, .. } => &context.mechanism,
    };
    let ScopeMechanismIdentity::LinuxHelper(mechanism) = &mechanism else {
        anyhow::bail!("native group is not a Linux helper mechanism");
    };
    anyhow::ensure!(
        mechanism.executable_device == image.dev()
            && mechanism.executable_inode == image.ino()
            && mechanism.protocol_version == 1,
        "unregistered helper image/protocol"
    );
    let HelperPlan::Execution {
        context,
        specification,
        environment,
    } = plan;
    let mut command = Command::new(&specification.program);
    command
        .args(&specification.args)
        .env_clear()
        .envs(environment.decode()?)
        .envs(specification.environment.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // SAFETY: the child changes only its own exec-surviving descriptor set.
    unsafe {
        command.pre_exec(super::process::mark_child_descriptors_cloexec);
    }
    before_spawn(&mut command);
    // Readiness precedes the target exec handshake. The helper remains able to forward
    // Stop signals while std waits for exec (including a slow filesystem/loader).
    send(&mut report, &context, &HelperEvent::Ready);
    let (spawned, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = spawned.send(command.spawn());
    });
    let result = loop {
        match receive.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("target spawn worker ended")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => forward_signal()?,
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut child = match result {
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
    after_spawn();
    release_pipe_stdin()?;
    send(&mut report, &context, &HelperEvent::Started);
    let mut status = None;
    let mut root_exited = false;
    loop {
        if !root_exited && let Some(exit) = child.try_wait()? {
            status = exit.code();
            root_exited = true;
            send(&mut report, &context, &HelperEvent::LeaderExited { status });
        }
        forward_signal()?;
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

/// After spawn only the target may retain the pipe reader. Keeping the helper's
/// copy would hide a target half-close and falsely acknowledge subsequent writes.
fn release_pipe_stdin() -> anyhow::Result<()> {
    // SAFETY: fstat initializes this output and only inspects the helper's fd 0.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    anyhow::ensure!(
        unsafe { libc::fstat(libc::STDIN_FILENO, &mut stat) } == 0,
        "helper stdin inspection failed: {}",
        std::io::Error::last_os_error()
    );
    if stat.st_mode & libc::S_IFMT == libc::S_IFIFO {
        let null = File::open("/dev/null")?;
        // SAFETY: target spawn has completed; replace only the helper's stdin.
        // PTY/non-pipe stdin and both output descriptors retain their ownership.
        anyhow::ensure!(
            unsafe { libc::dup2(null.as_raw_fd(), libc::STDIN_FILENO) } >= 0,
            "helper stdin release failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
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
    let mut children = Vec::new();
    // A target in std's exec handshake belongs to the spawning thread until it exits.
    for task in std::fs::read_dir("/proc/self/task")? {
        let path = task?.path().join("children");
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for pid in contents.split_whitespace() {
            children.push(pid.parse()?);
        }
    }
    Ok(children)
}

fn forward_signal() -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    let signal = REQUESTED_SIGNAL.load(Ordering::SeqCst);
    if signal == 0 {
        return Ok(());
    }
    for pid in immediate_children()? {
        // Pin first, then revalidate scope membership: the spawn worker can reap an exec failure.
        // No signal is ever delivered through a recycled numeric PID.
        // SAFETY: opening a native handle has no signaling or reaping side effect.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful pidfd_open transferred this unique descriptor.
        let handle = unsafe { File::from_raw_fd(fd as RawFd) };
        if !immediate_children()?.contains(&pid) {
            continue;
        }
        // SAFETY: the owned pidfd was pinned before scope membership was revalidated.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                handle.as_raw_fd(),
                if signal == libc::SIGUSR1 {
                    libc::SIGKILL
                } else {
                    signal
                },
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(std::io::Error::last_os_error().into());
        }
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

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn w_owned_none_helper_plan_matches_released_bytes() {
        // Serialized by the production HelperPlan at 88e8615c0 from the same
        // released linux-v4 journal used by the journal contract fixtures.
        let bytes = include_bytes!("fixtures/linux-v10.explicit.helper.json");
        let plan: HelperPlan = serde_json::from_slice(bytes).unwrap();
        assert_eq!(serde_json::to_vec(&plan).unwrap(), bytes);
    }
}
