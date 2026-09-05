//! Trusted host-side owner of one namespace and its actual init pidfd.
use crate::init::StartMessage;
use crate::linux::{self, Observation, PidFd};
use crate::storage::{self, Lock, Phase};
use crate::transport::Transport;
use crate::{BoundaryHandle, Error, QuiescenceProof, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// On every exit path close the gate first, terminate namespace init if known,
/// and reap our bwrap child. Caller must still establish proof, not trust Drop.
struct Children {
    bwrap: Child,
    init: Option<PidFd>,
    gate: Option<File>,
}
impl Drop for Children {
    fn drop(&mut self) {
        self.gate.take();
        if let Some(init) = &self.init {
            let _ = init.kill();
        }
        let _ = self.bwrap.kill();
        let _ = self.bwrap.wait();
    }
}

pub(crate) fn run(directory: &Path) -> Result<()> {
    let directory = directory.canonicalize()?;
    // Lifetime lock prevents a second launcher from sharing the physical run.
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("owner.lock"))?;
    if unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::Conflict("run already has a launcher".into()));
    }
    let mut record;
    let mut children;
    let mut transport;
    {
        let _lock = Lock::acquire(&directory)?;
        record = storage::read(&directory)?;
        if !matches!(record.phase, Phase::Reserved) {
            return Err(Error::Conflict("launcher cannot recreate used run".into()));
        }
        // Consume the launch reservation before spawning. A crash before identity
        // capture leaves incomplete evidence, never permission for another launch.
        let consumed = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("launch-consumed"))?;
        consumed.sync_all()?;
        File::open(&directory)?.sync_all()?;

        let (gate_read, gate_write) = linux::pipe()?;
        let (info_read, info_write) = linux::pipe()?;
        let (stdout_read, stdout_write) = linux::pipe()?;
        let mut command =
            linux::base_command(&record.bwrap, &record.helper, record.launch_config.network);
        command
            .arg("--bind")
            .arg(&record.launch_config.workspace)
            .arg("/workspace");
        for mount in &record.launch_config.mounts {
            command
                .arg(if mount.writable {
                    "--bind"
                } else {
                    "--ro-bind"
                })
                .arg(&mount.source)
                .arg(&mount.destination);
        }
        command
            .args([
                "--chdir",
                "/workspace",
                "--info-fd",
                "4",
                "--",
                "/boundary-helper",
                "init",
            ])
            .arg(&record.token)
            .stdin(Stdio::piped())
            // Provider stdout is FD 5, so the outer bwrap monitor never retains
            // a write copy that would hide provider EOF while init remains live.
            .stdout(Stdio::null())
            .stderr(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(directory.join("provider.stderr"))?,
            );
        install_fds(&mut command, &gate_read, &info_write, &stdout_write)?;
        let bwrap = command.spawn()?;
        // Drop the pre_exec closure's duplicated write end before waiting for EOF.
        drop(command);
        drop(gate_read);
        drop(info_write);
        drop(stdout_write);
        children = Children {
            bwrap,
            init: None,
            gate: Some(File::from(gate_write)),
        };
        let pid = read_child_pid(info_read)?;
        let pinned = PidFd::open(pid)?;
        let identity = linux::identity(pid, true)?;
        linux::verify_capture_parent(pid, children.bwrap.id())?;
        linux::verify_network(pid, record.launch_config.network)?;
        if pinned.exited()? {
            return Err(Error::Evidence(
                "namespace init exited during capture".into(),
            ));
        }
        children.init = Some(pinned);
        let handle = BoundaryHandle {
            run_id: record.run_id.clone(),
            attempt_id: record.launch_config.attempt_id.clone(),
            config_digest: record.config_digest.clone(),
            init: identity,
        };
        transport = Transport::new(
            &directory,
            children
                .bwrap
                .stdin
                .take()
                .ok_or_else(|| Error::Evidence("missing stdin".into()))?,
            File::from(stdout_read),
        )?;
        record.phase = Phase::Prepared {
            handle,
            launcher_pid: std::process::id() as i32,
        };
        storage::write(&directory, &record)?;
    }
    let handle = record.handle().expect("prepared handle").clone();
    loop {
        {
            let _lock = Lock::acquire(&directory)?;
            record = storage::read(&directory)?;
            record.check_handle(&handle)?;
            match &record.phase {
                Phase::Running { .. } => {
                    if let Some(mut gate) = children.gate.take() {
                        let bytes = serde_json::to_vec(&StartMessage {
                            token: record.token.clone(),
                            launch_config: record.launch_config.clone(),
                        })?;
                        send_start(&mut gate, &bytes)?;
                    }
                }
                Phase::Closing { .. } => {
                    children.gate.take();
                    children.init.as_ref().expect("captured pidfd").kill()?;
                }
                Phase::Closed(_) => break,
                Phase::Prepared { .. } => {}
                Phase::Reserved => return Err(Error::Evidence("run evidence regressed".into())),
            }
        }
        // Actual init identity is checked even when the outer process exits early.
        children.bwrap.try_wait()?;
        match linux::observe(&handle.init) {
            Observation::Gone(method) => {
                let _lock = Lock::acquire(&directory)?;
                let mut latest = storage::read(&directory)?;
                latest.check_handle(&handle)?;
                if !matches!(latest.phase, Phase::Closed(_)) {
                    latest.phase = Phase::Closed(QuiescenceProof {
                        handle: handle.clone(),
                        observed_at_ms: storage::now_ms(),
                        method: method.into(),
                    });
                    storage::write(&directory, &latest)?;
                }
                break;
            }
            Observation::Unknown(reason) => return Err(Error::Evidence(reason)),
            Observation::Live(_) => {}
        }
        transport.step()?;
        std::thread::sleep(Duration::from_millis(5));
    }
    transport.finish(&directory)
}

fn install_fds(
    command: &mut std::process::Command,
    gate: &OwnedFd,
    info: &OwnedFd,
    output: &OwnedFd,
) -> Result<()> {
    // Duplicate high first: source FDs may themselves be 3 or 4.
    let high = |fd| -> Result<OwnedFd> {
        use std::os::fd::FromRawFd;
        let copied = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        if copied < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(copied) })
    };
    let gate = high(gate.as_raw_fd())?;
    let info = high(info.as_raw_fd())?;
    let output = high(output.as_raw_fd())?;
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(gate.as_raw_fd(), 3) < 0
                || libc::dup2(info.as_raw_fd(), 4) < 0
                || libc::dup2(output.as_raw_fd(), 5) < 0
                || libc::syscall(
                    libc::SYS_close_range,
                    6u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

fn read_child_pid(fd: OwnedFd) -> Result<i32> {
    linux::nonblocking(fd.as_raw_fd())?;
    let mut file = File::from(fd);
    let mut data = Vec::new();
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        let mut chunk = [0; 1024];
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                data.extend_from_slice(&chunk[..count]);
                if data.len() > 8192 {
                    return Err(Error::Evidence("oversized bwrap identity".into()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= until {
            return Err(Error::Evidence("bwrap identity timeout".into()));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let value: serde_json::Value = serde_json::from_slice(&data)?;
    value
        .get("child-pid")
        .and_then(serde_json::Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| Error::Evidence("missing actual init PID".into()))
}

fn send_start(gate: &mut File, bytes: &[u8]) -> Result<()> {
    if bytes.len() > 1_048_576 {
        return Err(Error::Conflict("start message exceeds limit".into()));
    }
    linux::nonblocking(gate.as_raw_fd())?;
    let mut packet = Vec::with_capacity(bytes.len() + 4);
    packet.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    packet.extend_from_slice(bytes);
    let mut remaining = packet.as_slice();
    let until = Instant::now() + Duration::from_secs(1);
    while !remaining.is_empty() {
        match gate.write(remaining) {
            Ok(0) => return Err(Error::Evidence("start gate closed".into())),
            Ok(count) => remaining = &remaining[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= until {
            return Err(Error::Evidence("start gate did not drain".into()));
        }
        if !remaining.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}
