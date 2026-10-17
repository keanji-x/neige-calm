use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use calm_session::control::{IoMode, ProcSignal, execution::ExecutionSpecification};
use workspace::execution::NativeProcessIdentity;

use super::{
    native_child::OwnedChild,
    scope_helper::{HelperContext, HelperPlan},
};

pub(super) struct Process {
    pub started: std::sync::atomic::AtomicBool,
    pub child: Mutex<OwnedChild>,
    pub input: Mutex<File>,
    pub output: Mutex<Output>,
    pub pty: Option<Mutex<File>>,
    pidfd: File,
}

pub(super) struct Output {
    bytes: std::collections::VecDeque<u8>,
    cursor: u64,
    limit: usize,
}

impl Output {
    fn append(&mut self, bytes: &[u8]) {
        self.cursor += bytes.len() as u64;
        self.bytes.extend(bytes);
        while self.bytes.len() > self.limit {
            self.bytes.pop_front();
        }
    }
    pub fn snapshot(&self, cursor: Option<u64>) -> (u64, Vec<u8>) {
        let start = self.cursor - self.bytes.len() as u64;
        let offset = cursor
            .unwrap_or(start)
            .saturating_sub(start)
            .min(self.bytes.len() as u64) as usize;
        (
            self.cursor,
            self.bytes.iter().skip(offset).copied().collect(),
        )
    }
}

impl Process {
    pub fn signal(&self, signal: ProcSignal) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.started.load(std::sync::atomic::Ordering::SeqCst),
            "helper signal handler is not ready; retry exact control"
        );
        let signal = match signal {
            ProcSignal::Term => libc::SIGTERM,
            ProcSignal::Hup => libc::SIGHUP,
            ProcSignal::Kill => libc::SIGUSR1,
        };
        // Signal the owned helper through its pinned native handle; it drains every adopted child.
        // SAFETY: pidfd is owned by this exact execution, never a numeric alias lookup.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        anyhow::ensure!(
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "signal execution helper: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    pub fn write(&self, bytes: &[u8]) -> anyhow::Result<usize> {
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "execution input too large");
        let mut input = self
            .input
            .lock()
            .map_err(|_| anyhow::anyhow!("input poisoned"))?;
        loop {
            match input.write(bytes) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(0),
                Err(error) => return Err(error.into()),
            }
        }
    }
    pub fn resize(&self, cols: u16, rows: u16, pixel_w: u16, pixel_h: u16) -> anyhow::Result<()> {
        let master = self
            .pty
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("execution has no PTY"))?
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY poisoned"))?;
        let size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: pixel_w,
            ws_ypixel: pixel_h,
        };
        // SAFETY: owned PTY descriptor and initialized winsize.
        anyhow::ensure!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == 0,
            "resize PTY: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}

/// std's exec handshake proves spawn errors have reaped the failed executable child.
#[cfg(test)]
pub(super) fn spawn(
    helper: &File,
    cwd: &File,
    specification: &ExecutionSpecification,
    context: HelperContext,
) -> anyhow::Result<(Arc<Process>, NativeProcessIdentity, File)> {
    spawn_with_reader_hook(helper, cwd, specification, context, || {})
}

pub(super) fn spawn_with_reader_hook(
    helper: &File,
    cwd: &File,
    specification: &ExecutionSpecification,
    context: HelperContext,
    before_read: impl FnOnce() + Send + 'static,
) -> anyhow::Result<(Arc<Process>, NativeProcessIdentity, File)> {
    spawn_with_pin(helper, cwd, specification, context, pin_helper, before_read)
}

fn spawn_with_pin(
    helper: &File,
    cwd: &File,
    specification: &ExecutionSpecification,
    context: HelperContext,
    pin: impl FnOnce(u32) -> anyhow::Result<File>,
    before_read: impl FnOnce() + Send + 'static,
) -> anyhow::Result<(Arc<Process>, NativeProcessIdentity, File)> {
    let (configuration, config_writer) = pipe()?;
    let (reports, report_writer) = pipe()?;
    let nonce = context.nonce.clone();
    let encoded = serde_json::to_vec(&HelperPlan::Execution {
        context,
        specification: specification.clone(),
    })?;
    anyhow::ensure!(
        encoded.len() <= 1024 * 1024,
        "execution specification too large"
    );
    let mut command = helper_command(helper, &configuration, &report_writer, &[]);
    let (input, output, pty) = match specification.io_mode {
        IoMode::Pipe => {
            let (input_read, input_write) = pipe()?;
            let (output_read, output_write) = pipe()?;
            command
                .stdin(Stdio::from(input_read))
                .stdout(Stdio::from(output_write.try_clone()?))
                .stderr(Stdio::from(output_write));
            (input_write, output_read, None)
        }
        IoMode::Pty { cols, rows } => {
            let mut master = -1;
            let mut slave = -1;
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: output pointers are valid and the initial winsize is initialized.
            anyhow::ensure!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        &size,
                    )
                } == 0,
                "allocate execution PTY: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: successful openpty returned two owned descriptors.
            let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            cloexec(master.as_raw_fd(), true)?;
            cloexec(slave.as_raw_fd(), true)?;
            command
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            (
                master.try_clone()?,
                master.try_clone()?,
                Some(Mutex::new(master)),
            )
        }
    };
    let directory_fd = cwd.as_raw_fd();
    let is_pty = matches!(specification.io_mode, IoMode::Pty { .. });
    // SAFETY: pre_exec uses only async-signal-safe libc operations on owned descriptors.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(directory_fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if is_pty && libc::ioctl(0, libc::TIOCSCTTY, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = OwnedChild::new(command.spawn()?);
    drop(configuration);
    drop(report_writer);
    let pid = child.id();
    let pidfd = pin(pid)?;
    let identity = NativeProcessIdentity {
        pid,
        start_time_ticks: start_time(pid)?,
        nonce,
    };
    nonblocking(input.as_raw_fd())?;
    let process = Arc::new(Process {
        started: std::sync::atomic::AtomicBool::new(false),
        child: Mutex::new(child),
        input: Mutex::new(input),
        output: Mutex::new(Output {
            bytes: Default::default(),
            cursor: 0,
            limit: specification.replay_bytes.clamp(1024, 4 * 1024 * 1024),
        }),
        pty,
        pidfd,
    });
    std::thread::spawn(move || {
        let mut writer = config_writer;
        let _ = writer.write_all(&encoded);
    });
    let reader_process = process.clone();
    std::thread::spawn(move || {
        before_read();
        let mut reader = output;
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    if let Ok(mut output) = reader_process.output.lock() {
                        output.append(&bytes[..count]);
                    } else {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut descriptor = libc::pollfd {
                        fd: reader.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: polls one owned descriptor; a timeout is no exit/drain evidence.
                    unsafe {
                        libc::poll(&mut descriptor, 1, 100);
                    }
                }
                Err(_) => break,
            }
        }
    });
    Ok((process, identity, reports))
}

/// The one registered helper image/bootstrap path; roles declare their exact inherited FD set.
pub(super) fn helper_command(
    helper: &File,
    configuration: &File,
    reports: &File,
    inherited: &[RawFd],
) -> Command {
    let mut command = Command::new(format!("/proc/self/fd/{}", helper.as_raw_fd()));
    command
        .args([
            "--execution-helper",
            &configuration.as_raw_fd().to_string(),
            &reports.as_raw_fd().to_string(),
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut allowed = vec![configuration.as_raw_fd(), reports.as_raw_fd()];
    allowed.extend_from_slice(inherited);
    // SAFETY: executed after fork, only fcntl/close_range on the exact owned bootstrap FDs.
    unsafe {
        command.pre_exec(move || {
            mark_child_descriptors_cloexec()?;
            for fd in &allowed {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command
}

/// Child-only allowlist: stdio and explicitly reopened helper channels are the only exec survivors.
pub(super) fn mark_child_descriptors_cloexec() -> std::io::Result<()> {
    // SAFETY: called only after fork in the child; no host descriptor flags are changed.
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn pipe() -> std::io::Result<(File, File)> {
    let mut fds = [-1; 2];
    // SAFETY: pipe2 writes exactly two descriptors into this array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful pipe2 transferred both descriptors.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

pub(super) fn cloexec(fd: RawFd, enabled: bool) -> std::io::Result<()> {
    // SAFETY: fcntl acts on a descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: updates only the close-on-exec flag.
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFD,
            if enabled {
                flags | libc::FD_CLOEXEC
            } else {
                flags & !libc::FD_CLOEXEC
            },
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: both calls operate on an owned process input descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn start_time(pid: u32) -> anyhow::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let tail = stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("invalid process stat"))?
        .1;
    Ok(tail
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("missing process start time"))?
        .parse()?)
}

fn pin_helper(pid: u32) -> anyhow::Result<File> {
    // SAFETY: a positive child PID still pinned by the unreaped Child handle.
    let native_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    anyhow::ensure!(
        native_fd >= 0,
        "pin execution helper: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: pidfd_open returned a uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(native_fd as RawFd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace::execution::{
        CapabilityToken, ExecutionId, ExecutorBootId, ScopeMechanismIdentity,
    };

    #[test]
    fn execution_post_spawn_pin_failure_reaps_exact_child() {
        let helper = File::open("/bin/sh").unwrap();
        let cwd = File::open("/").unwrap();
        let context = HelperContext {
            execution_id: ExecutionId::fresh().unwrap(),
            executor_boot: ExecutorBootId::fresh().unwrap(),
            nonce: CapabilityToken::fresh().unwrap(),
            mechanism: ScopeMechanismIdentity {
                executable_device: 0,
                executable_inode: 0,
                protocol_version: 1,
            },
        };
        let specification = ExecutionSpecification {
            program: "/bin/true".into(),
            args: vec![],
            environment: vec![],
            io_mode: IoMode::Pipe,
            replay_bytes: 1024,
        };
        let mut spawned = None;
        let result = spawn_with_pin(
            &helper,
            &cwd,
            &specification,
            context,
            |pid| {
                spawned = Some(pid);
                Err(std::io::Error::from_raw_os_error(libc::EMFILE).into())
            },
            || {},
        );
        assert!(result.is_err());
        let pid = spawned.expect("fault must occur after a successful real spawn");
        let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // A non-consuming exact query establishes collection, rather than merely a stop request.
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut status,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
