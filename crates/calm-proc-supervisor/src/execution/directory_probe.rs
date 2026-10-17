//! One read-only role of the registered helper image. It never forks or executes a payload.
use super::{native_child::OwnedChild, process, scope_helper::HelperPlan};
use calm_session::control::execution::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    time::{Duration, Instant},
};
use workspace::execution::objects::*;

pub(super) const OBSERVATION_DEADLINE: Duration = Duration::from_secs(4);
const FRAME_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum TransferredDirectoryRole {
    Ancestor,
    Subject,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ProbeOrigin {
    Registered {
        registration: ObjectRegistrationId,
        incarnation: SourceObjectIncarnation,
    },
    Transferred {
        role: TransferredDirectoryRole,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProbeRoot {
    pub origin: ProbeOrigin,
    pub role: ObjectRole,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProbeContext {
    pub executor_boot: ExecutorBootId,
    pub nonce: TransferNonce,
    pub ancestor: ProbeRoot,
    pub subject: ProbeRoot,
    pub facts: DirectoryPairFacts,
    pub mechanism: ScopeMechanismIdentity,
}
impl ProbeContext {
    fn origins_valid(&self) -> bool {
        matches!(
            (&self.ancestor.origin, &self.subject.origin),
            (
                ProbeOrigin::Registered { .. },
                ProbeOrigin::Registered { .. }
            ) | (
                ProbeOrigin::Transferred {
                    role: TransferredDirectoryRole::Ancestor
                },
                ProbeOrigin::Transferred {
                    role: TransferredDirectoryRole::Subject
                }
            )
        )
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct ProbePlan {
    pub context: ProbeContext,
    pub directories: [RawFd; 2],
}
#[derive(Serialize, Deserialize)]
struct ProbeReport {
    context: ProbeContext,
    outcome: DirectoryRelationOutcome,
}

fn unknown(reason: DirectoryRelationUnknown) -> DirectoryRelationOutcome {
    DirectoryRelationOutcome::Unknown(reason)
}
pub(super) fn pair_failure(failure: DirectoryPairFailure) -> DirectoryRelationOutcome {
    match failure {
        DirectoryPairFailure::Unsupported(ContainmentUnsupported::DifferentMount) => {
            DirectoryRelationOutcome::Unsupported(DirectoryRelationUnsupported::DifferentMount)
        }
        DirectoryPairFailure::Unsupported(_) => {
            DirectoryRelationOutcome::Unsupported(DirectoryRelationUnsupported::Platform)
        }
        DirectoryPairFailure::Unknown(ContainmentUnknown::DetachedDirectory) => {
            unknown(DirectoryRelationUnknown::DetachedDirectory)
        }
        DirectoryPairFailure::Unknown(_) => unknown(DirectoryRelationUnknown::ObjectsChanged),
    }
}

/// This blocking job retains the exact Child and both source FDs until concrete-PID reap.
/// If an abnormal kernel wait stalls, the async RPC timeout returns Unknown; this owner remains
/// responsible for the child. It never publishes a completed/reaped fact on that timeout.
pub(super) fn observe(
    helper: File,
    directories: [File; 2],
    context: ProbeContext,
) -> DirectoryRelationOutcome {
    let result = observe_inner(&helper, &directories, &context);
    match current_directory_pair(&directories[0], &directories[1]) {
        Ok(facts) if facts == context.facts => result,
        Ok(_) => unknown(DirectoryRelationUnknown::ObjectsChanged),
        Err(failure) => pair_failure(failure),
    }
}

fn observe_inner(
    helper: &File,
    directories: &[File; 2],
    context: &ProbeContext,
) -> DirectoryRelationOutcome {
    let run = || -> std::io::Result<DirectoryRelationOutcome> {
        let (configuration, mut writer) = process::pipe()?;
        let (mut reports, report_writer) = process::pipe()?;
        let inherited = [directories[0].as_raw_fd(), directories[1].as_raw_fd()];
        let plan = HelperPlan::ObserveDirectories(ProbePlan {
            context: context.clone(),
            directories: inherited,
        });
        let encoded = serde_json::to_vec(&plan)?;
        if encoded.len() > FRAME_LIMIT {
            return Ok(unknown(DirectoryRelationUnknown::HelperProtocol));
        }
        let mut command =
            process::helper_command(helper, &configuration, &report_writer, &inherited);
        let parent = std::process::id();
        // SAFETY: only async-signal-safe syscalls in the post-fork child; no allocation/FS lookup.
        unsafe {
            command.pre_exec(move || {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent as libc::pid_t
                {
                    return Err(std::io::Error::last_os_error());
                }
                libc::alarm(5);
                Ok(())
            });
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return Ok(unknown(DirectoryRelationUnknown::HelperSpawn)),
        };
        let mut child = OwnedChild::new(child);
        drop(configuration);
        drop(report_writer);
        // One bounded frame fits the empty pipe's atomic-write capacity. There is no writer thread.
        writer.write_all(&encoded)?;
        drop(writer);
        process::nonblocking(reports.as_raw_fd())?;
        collect(&mut child, &mut reports, context)
    };
    run().unwrap_or_else(|_| unknown(DirectoryRelationUnknown::HelperIo))
}

fn collect(
    child: &mut OwnedChild,
    reports: &mut File,
    context: &ProbeContext,
) -> std::io::Result<DirectoryRelationOutcome> {
    let deadline = Instant::now() + OBSERVATION_DEADLINE;
    let mut bytes = Vec::new();
    let mut exited = None;
    let mut eof = false;
    loop {
        let mut buffer = [0; 1024];
        match reports.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > FRAME_LIMIT {
                    return Ok(unknown(DirectoryRelationUnknown::HelperProtocol));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        if let Some(status) = exited {
            if !status.success() {
                return Ok(unknown(DirectoryRelationUnknown::HelperExited));
            }
            if eof {
                return Ok(match serde_json::from_slice::<ProbeReport>(&bytes) {
                    Ok(report) if report.context == *context => report.outcome,
                    _ => unknown(DirectoryRelationUnknown::HelperProtocol),
                });
            }
        }
        if Instant::now() >= deadline {
            return Ok(unknown(DirectoryRelationUnknown::Timeout));
        }
        let mut descriptor = libc::pollfd {
            fd: reports.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one owned report pipe; polling never consumes or reaps a host child.
        unsafe { libc::poll(&mut descriptor, 1, 10) };
    }
}

pub(super) fn run(plan: ProbePlan, mut report: File, reserved: [RawFd; 2]) -> anyhow::Result<()> {
    let [ancestor, subject] = plan.directories;
    anyhow::ensure!(
        ancestor > 2
            && subject > 2
            && ancestor != subject
            && !reserved.contains(&ancestor)
            && !reserved.contains(&subject),
        "invalid observer FD roles"
    );
    // SAFETY: only this registered helper role inherited these distinct, owned directory FDs.
    let directories = unsafe { [File::from_raw_fd(ancestor), File::from_raw_fd(subject)] };
    let outcome = match current_directory_pair(&directories[0], &directories[1]) {
        Ok(facts)
            if facts == plan.context.facts
                && plan.context.ancestor.role == ObjectRole::Root
                && plan.context.subject.role == ObjectRole::Root
                && plan.context.origins_valid() =>
        {
            raw_observation(&directories, &facts)
        }
        Ok(_) => unknown(DirectoryRelationUnknown::ObjectsChanged),
        Err(failure) => pair_failure(failure),
    };
    serde_json::to_writer(
        &mut report,
        &ProbeReport {
            context: plan.context,
            outcome,
        },
    )?;
    report.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn raw_observation(
    directories: &[File; 2],
    facts: &DirectoryPairFacts,
) -> DirectoryRelationOutcome {
    // Linux >=3.9 CLONE_NEWUSER also gives this single-threaded child a private fs_struct.
    // No mount/PID namespace, mappings, host capability, or external command is involved.
    if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
        return DirectoryRelationOutcome::Unsupported(
            DirectoryRelationUnsupported::UserNamespace {
                errno: std::io::Error::last_os_error()
                    .raw_os_error()
                    .expect("unshare errno"),
            },
        );
    }
    // User-namespace credential changes must not reopen access to the private report channel.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
        return syscall_failure(DirectoryObservationStage::Namespace);
    }
    if unsafe { libc::fchdir(directories[0].as_raw_fd()) } != 0 {
        return syscall_failure(DirectoryObservationStage::AncestorCwd);
    }
    if unsafe { libc::chroot(c".".as_ptr()) } != 0 {
        return DirectoryRelationOutcome::Unsupported(DirectoryRelationUnsupported::RootChange {
            errno: std::io::Error::last_os_error()
                .raw_os_error()
                .expect("chroot errno"),
        });
    }
    if unsafe { libc::fchdir(directories[1].as_raw_fd()) } != 0 {
        return syscall_failure(DirectoryObservationStage::SubjectCwd);
    }
    let mut buffer = [0u8; 4096];
    // Linux v6.1 fs/d_path.c: raw getcwd retains the '(unreachable)' marker and retries
    // rename_lock/mount_lock sequences. libc getcwd may instead hide it with a fallback.
    let count = unsafe { libc::syscall(libc::SYS_getcwd, buffer.as_mut_ptr(), buffer.len()) };
    if count < 0 {
        return syscall_failure(DirectoryObservationStage::GetCwd);
    }
    for (file, expected) in directories.iter().zip([&facts.ancestor, &facts.subject]) {
        match file.metadata() {
            Ok(metadata) if metadata.nlink() == 0 => {
                return unknown(DirectoryRelationUnknown::DetachedDirectory);
            }
            Ok(metadata)
                if metadata.is_dir()
                    && metadata.dev() == expected.device
                    && metadata.ino() == expected.inode => {}
            _ => return unknown(DirectoryRelationUnknown::ObjectsChanged),
        }
    }
    let Ok(count) = usize::try_from(count) else {
        return unknown(DirectoryRelationUnknown::HelperProtocol);
    };
    if count == 0
        || count > buffer.len()
        || buffer[count - 1] != 0
        || buffer[..count - 1].contains(&0)
    {
        return unknown(DirectoryRelationUnknown::HelperProtocol);
    }
    match &buffer[..count - 1] {
        b"/" => DirectoryRelationOutcome::Contains(ContainmentKind::SameDirectory),
        path if path.starts_with(b"/") => {
            DirectoryRelationOutcome::Contains(ContainmentKind::StrictDescendant)
        }
        path if path.starts_with(b"(unreachable)/") => DirectoryRelationOutcome::Disjoint,
        _ => unknown(DirectoryRelationUnknown::HelperProtocol),
    }
}

#[cfg(not(target_os = "linux"))]
fn raw_observation(_: &[File; 2], _: &DirectoryPairFacts) -> DirectoryRelationOutcome {
    DirectoryRelationOutcome::Unsupported(DirectoryRelationUnsupported::Platform)
}

fn syscall_failure(stage: DirectoryObservationStage) -> DirectoryRelationOutcome {
    unknown(DirectoryRelationUnknown::SystemCall {
        stage,
        errno: std::io::Error::last_os_error()
            .raw_os_error()
            .expect("syscall errno"),
    })
}

#[cfg(test)]
mod tests;
