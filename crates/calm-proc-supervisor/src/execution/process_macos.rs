//! Darwin owns the payload child directly. Its unreaped leader pins only the original group.
use calm_session::control::{IoMode, ProcSignal, execution::*};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

pub(super) struct Process {
    pub ready: AtomicBool,
    pub stop_deadline: Mutex<Option<tokio::time::Instant>>,
    ownership: Mutex<Ownership>,
    input: Mutex<Option<File>>,
}
/// Launch retains this guard through receipt persistence and resume. Any early
/// return cleans the very same owner even after insertion into the process map.
pub(super) struct StartupCleanup(pub Option<Arc<Process>>);
impl Drop for StartupCleanup {
    fn drop(&mut self) {
        if let Some(process) = &self.0
            && let Err(error) = process.cleanup()
        {
            tracing::error!(%error, "native startup cleanup incomplete");
        }
    }
}
#[path = "native_owned_child.rs"]
mod native_child;
use native_child::{Child, Pin};
struct Ownership {
    identity: MacProcessIdentity,
    child: Child,
}
pub(super) enum Observation {
    Running,
    Exited { status: Option<i32>, empty: bool },
}
impl Ownership {
    fn observe(&mut self) -> anyhow::Result<(libc::proc_bsdinfo, Option<Option<i32>>)> {
        // The child owner validates the retained pin before observing the OS.
        let info = self.child.wait_info()?;
        #[cfg(test)]
        tests::fault(tests::Fault::Observe)?;
        let native = process_info(self.identity.pid)?;
        if native.pbi_pid != self.identity.pid
            || native.pbi_start_tvsec != self.identity.birth_seconds
            || native.pbi_start_tvusec != self.identity.birth_microseconds
        {
            self.child.pin = Pin::Lost;
            anyhow::bail!("original leader identity changed; numeric signals prohibited");
        }
        anyhow::ensure!(
            native.pbi_pgid == self.identity.pid,
            "original leader group changed"
        );
        let status = wait_status(&info, self.identity.pid)?;
        Ok((native, status))
    }
    fn members(&mut self) -> anyhow::Result<Vec<libc::proc_bsdinfo>> {
        self.observe()?;
        let result = group_members(&self.identity);
        // A failed snapshot grants no signal authority. Preserve ownership for diagnostics;
        // a future observation can retry, but this one cannot certify completion.
        result
    }
}
impl Process {
    /// Failure cleanup uses the same child and lock as normal signals/reap, but
    /// never relies on lifecycle decoding or a successful group snapshot.
    pub(super) fn cleanup(&self) -> anyhow::Result<()> {
        self.ownership
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .cleanup()
    }
    pub fn resume(&self) -> anyhow::Result<()> {
        let mut owner = self.ownership.lock().map_err(super::poison)?;
        let (_, status) = owner.observe()?;
        anyhow::ensure!(status.is_none(), "native child exited before resume");
        owner.child.signal(false, libc::SIGCONT)?;
        Ok(())
    }
    pub fn signal(&self, signal: ProcSignal) -> anyhow::Result<()> {
        let mut owner = self.ownership.lock().map_err(super::poison)?;
        if matches!(owner.child.pin, Pin::Reaped) {
            return Ok(());
        }
        let members = owner.members()?;
        // Zombie-only groups have nothing to signal (Darwin may answer EPERM for them).
        if !members.iter().any(|p| p.pbi_status != libc::SZOMB) {
            return Ok(());
        }
        let signal = match signal {
            ProcSignal::Term => libc::SIGTERM,
            ProcSignal::Kill => libc::SIGKILL,
            ProcSignal::Hup => libc::SIGHUP,
        };
        // The exact child pin is still held; reap and every signal use this same lock.
        owner.child.signal(true, signal)
    }
    pub fn poll(&self) -> anyhow::Result<Observation> {
        let mut owner = self.ownership.lock().map_err(super::poison)?;
        let (_, status) = owner.observe()?;
        let Some(status) = status else {
            return Ok(Observation::Running);
        };
        let members = owner.members()?;
        Ok(Observation::Exited {
            status,
            empty: members.len() == 1,
        })
    }
    pub fn reap(&self) -> anyhow::Result<()> {
        let mut owner = self.ownership.lock().map_err(super::poison)?;
        let (_, status) = owner.observe()?;
        anyhow::ensure!(status.is_some(), "cannot reap a running native root");
        anyhow::ensure!(
            owner.members()?.len() == 1,
            "original native group still occupied"
        );
        owner
            .child
            .record("group-empty", "only pinned root remains before reap");
        anyhow::ensure!(owner.child.reap_once()?, "native root not ready to reap");
        Ok(())
    }
    pub fn write(&self, bytes: &[u8]) -> anyhow::Result<usize> {
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "execution input too large");
        let mut input = self.input.lock().map_err(super::poison)?;
        let input = input
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("execution stdin closed"))?;
        loop {
            match input.write(bytes) {
                Ok(count) => return Ok(count),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(0),
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn close_stdin(&self) -> anyhow::Result<()> {
        self.input.lock().map_err(super::poison)?.take();
        Ok(())
    }
    pub fn resize(&self, _: u16, _: u16, _: u16, _: u16) -> anyhow::Result<()> {
        anyhow::bail!("native registered execution supports Pipe only")
    }
}

/// Darwin may report START_SUSPENDED as CLD_STOPPED even with WEXITED.
/// WNOWAIT keeps stop/continue notifications pending; neither is an exit fact.
fn wait_status(info: &libc::siginfo_t, pid: u32) -> anyhow::Result<Option<Option<i32>>> {
    if info.si_pid == 0 && info.si_signo == 0 && info.si_code == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        info.si_signo == libc::SIGCHLD && info.si_pid == pid as i32,
        "invalid waitid event: signo={} pid={} code={} status={}",
        info.si_signo,
        info.si_pid,
        info.si_code,
        info.si_status
    );
    match info.si_code {
        libc::CLD_EXITED => Ok(Some(Some(info.si_status))),
        libc::CLD_KILLED | libc::CLD_DUMPED => Ok(Some(None)),
        libc::CLD_STOPPED | libc::CLD_CONTINUED => Ok(None),
        _ => anyhow::bail!(
            "unexpected native wait status: code={} status={}",
            info.si_code,
            info.si_status
        ),
    }
}

fn process_info(pid: u32) -> anyhow::Result<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // arg=1 includes zombies: excluding the pinned leader would create a false empty result.
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            std::mem::size_of_val(&info) as i32,
        )
    };
    anyhow::ensure!(
        count == std::mem::size_of_val(&info) as i32,
        "native process information unavailable: {}",
        io::Error::last_os_error()
    );
    Ok(info)
}
fn group_members(identity: &MacProcessIdentity) -> anyhow::Result<Vec<libc::proc_bsdinfo>> {
    #[cfg(test)]
    tests::fault(tests::Fault::Group)?;
    let mut capacity = 32;
    loop {
        anyhow::ensure!(
            capacity <= 65536,
            "native group observation capacity exhausted"
        );
        let mut pids = vec![0i32; capacity];
        unsafe {
            *libc::__error() = 0;
        }
        // Darwin returns a PID count, while the buffer size argument is in bytes.
        let count = unsafe {
            libc::proc_listpgrppids(
                identity.pid as i32,
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        let errno = io::Error::last_os_error();
        anyhow::ensure!(count > 0, "native group observation failed/empty: {errno}");
        if count as usize >= capacity {
            capacity *= 2;
            continue;
        }
        pids.truncate(count as usize);
        anyhow::ensure!(
            pids.contains(&(identity.pid as i32)),
            "native group observation lost pinned leader"
        );
        let mut members = Vec::new();
        for pid in pids {
            anyhow::ensure!(pid > 0, "invalid native group member");
            let info = process_info(pid as u32)?;
            anyhow::ensure!(
                info.pbi_pid == pid as u32 && info.pbi_pgid == identity.pid,
                "native group changed during observation"
            );
            if pid as u32 == identity.pid {
                anyhow::ensure!(
                    info.pbi_start_tvsec == identity.birth_seconds
                        && info.pbi_start_tvusec == identity.birth_microseconds,
                    "native leader birth identity changed"
                );
            }
            if pid as u32 != identity.pid {
                tracing::trace!(
                    pid,
                    group = identity.pid,
                    birth_seconds = info.pbi_start_tvsec,
                    birth_microseconds = info.pbi_start_tvusec,
                    "native group member observed"
                );
                #[cfg(test)]
                {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(
                        stderr,
                        "native-member pid={pid} group={} birth={}.{}",
                        identity.pid, info.pbi_start_tvsec, info.pbi_start_tvusec
                    );
                    let _ = stderr.flush();
                }
            }
            members.push(info);
        }
        return Ok(members);
    }
}

unsafe extern "C" {
    fn posix_spawn_file_actions_addfchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: i32,
    ) -> i32;
}
fn check(rc: i32) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}
struct SpawnActions(libc::posix_spawn_file_actions_t);
impl Drop for SpawnActions {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawn_file_actions_destroy(&mut self.0);
        }
    }
}
struct SpawnAttributes(libc::posix_spawnattr_t);
impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawnattr_destroy(&mut self.0);
        }
    }
}
pub(super) fn spawn(
    cwd: &File,
    specification: &ExecutionSpecification,
    nonce: CapabilityToken,
    output: Arc<super::output::OutputLog>,
) -> anyhow::Result<(Arc<Process>, NativeProcessIdentity)> {
    anyhow::ensure!(
        matches!(specification.io_mode, IoMode::Pipe),
        "native registered execution supports Pipe only"
    );
    let program = CString::new(specification.program.as_os_str().as_encoded_bytes())?;
    let arguments = std::iter::once(program.clone())
        .chain(
            specification
                .args
                .iter()
                .map(|s| CString::new(s.as_bytes()))
                .collect::<Result<Vec<_>, _>>()?,
        )
        .collect::<Vec<_>>();
    let launch_environment = crate::child_environment::launch_environment(specification)?;
    let environment = launch_environment
        .0
        .iter()
        .map(|(key, value)| CString::new([key.as_slice(), b"=", value.as_slice()].concat()))
        .collect::<Result<Vec<_>, _>>()?;
    let argv = arguments
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();
    let envp = environment
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    nonblocking(input_write.as_raw_fd())?;
    nonblocking(output_read.as_raw_fd())?;
    let mut raw = unsafe { std::mem::zeroed() };
    check(unsafe { libc::posix_spawn_file_actions_init(&mut raw) })?;
    let mut actions = SpawnActions(raw);
    check(unsafe { posix_spawn_file_actions_addfchdir_np(&mut actions.0, cwd.as_raw_fd()) })?;
    for (source, target) in [
        (input_read.as_raw_fd(), 0),
        (output_write.as_raw_fd(), 1),
        (output_write.as_raw_fd(), 2),
    ] {
        check(unsafe { libc::posix_spawn_file_actions_adddup2(&mut actions.0, source, target) })?;
    }
    let mut raw = unsafe { std::mem::zeroed() };
    check(unsafe { libc::posix_spawnattr_init(&mut raw) })?;
    let mut attributes = SpawnAttributes(raw);
    let mut mask = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut mask);
    }
    check(unsafe { libc::posix_spawnattr_setsigmask(&mut attributes.0, &mask) })?;
    for signal in [
        libc::SIGCHLD,
        libc::SIGTERM,
        libc::SIGINT,
        libc::SIGHUP,
        libc::SIGQUIT,
        libc::SIGPIPE,
    ] {
        unsafe {
            libc::sigaddset(&mut mask, signal);
        }
    }
    check(unsafe { libc::posix_spawnattr_setsigdefault(&mut attributes.0, &mask) })?;
    check(unsafe { libc::posix_spawnattr_setpgroup(&mut attributes.0, 0) })?;
    check(unsafe {
        libc::posix_spawnattr_setflags(
            &mut attributes.0,
            (libc::POSIX_SPAWN_START_SUSPENDED
                | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                | libc::POSIX_SPAWN_SETPGROUP
                | libc::POSIX_SPAWN_SETSIGMASK
                | libc::POSIX_SPAWN_SETSIGDEF) as i16,
        )
    })?;
    let mut pid = 0;
    check(unsafe {
        libc::posix_spawn(
            &mut pid,
            program.as_ptr(),
            &actions.0,
            &attributes.0,
            argv.as_ptr(),
            envp.as_ptr(),
        )
    })?;
    let mut child = Child::new(pid);
    child.record(
        "spawn",
        &format!(
            "cwd={:?}",
            cwd.metadata().map(|m| {
                use std::os::unix::fs::MetadataExt;
                (m.dev(), m.ino())
            })
        ),
    );
    // No untrusted code executes in a fork callback. posix_spawn's file actions and error
    // return establish cwd/group/exec atomically before this concrete ownership is published.
    let identity = match process_info(pid as u32) {
        Ok(info) if info.pbi_pgid == pid as u32 => MacProcessIdentity {
            pid: pid as u32,
            birth_seconds: info.pbi_start_tvsec,
            birth_microseconds: info.pbi_start_tvusec,
            nonce,
        },
        _ => anyhow::bail!(
            "native spawn succeeded but root identity unavailable; native ownership requires reconciliation"
        ),
    };
    child.group = true;
    child.record(
        "identity",
        &format!(
            "birth={}.{} group={}",
            identity.birth_seconds, identity.birth_microseconds, pid
        ),
    );
    #[cfg(test)]
    tests::fault(tests::Fault::Setup)?;
    let process = Arc::new(Process {
        ready: AtomicBool::new(true),
        stop_deadline: Mutex::new(None),
        ownership: Mutex::new(Ownership {
            identity: identity.clone(),
            child,
        }),
        input: Mutex::new(Some(input_write)),
    });
    drop(input_read);
    drop(output_write);
    output.read(output_read, false);
    Ok((process, NativeProcessIdentity::MacOwnedGroup(identity)))
}
fn pipe() -> io::Result<(File, File)> {
    // Also excludes this platform's pipe()/fcntl window from legacy supervisor forks.
    let _inheritance = rights::inheritance_guard()?;
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let files = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    for file in [&files.0, &files.1] {
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(files)
}
fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn host_boot_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 128];
    let mut size = bytes.len();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    anyhow::ensure!(
        rc == 0 && size > 1 && size <= bytes.len(),
        "read native boot identity: {}",
        io::Error::last_os_error()
    );
    let text = std::ffi::CStr::from_bytes_with_nul(&bytes[..size])?.to_str()?;
    anyhow::ensure!(!text.is_empty(), "empty native boot identity");
    Ok(text.to_owned())
}

#[cfg(test)]
#[path = "process_macos_tests.rs"]
mod tests;
