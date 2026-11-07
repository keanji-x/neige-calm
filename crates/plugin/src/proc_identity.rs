//! Process identity observation shared by child cleanup and server supervision.
//!
//! Start identifiers are platform-specific: Linux uses clock ticks since boot;
//! macOS uses the kernel's process start timeval as microseconds since the Unix
//! epoch. Treat them as opaque equality tokens, paired with a boot ID when
//! persisted, not as durations or timestamps comparable across platforms.

/// Native observation of a physical PID, including unreaped zombies.
/// `Unknown` grants neither identity nor proof that the PID has disappeared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessObservation {
    Present { start_time: u64, is_zombie: bool },
    Absent,
    Unknown,
}

/// Observe Linux identity and state in one `/proc/<pid>/stat` read.
#[cfg(target_os = "linux")]
pub fn observe_process(pid: i32) -> ProcessObservation {
    if pid <= 0 {
        return ProcessObservation::Absent;
    }
    classify_proc_stat(std::fs::read_to_string(format!("/proc/{pid}/stat")))
}

#[cfg(any(target_os = "linux", test))]
fn classify_proc_stat(stat: std::io::Result<String>) -> ProcessObservation {
    match stat {
        Ok(stat) => match parse_proc_stat_fields(&stat) {
            Some(fields) => ProcessObservation::Present {
                start_time: fields.start_time,
                is_zombie: fields.state == 'Z',
            },
            None => ProcessObservation::Unknown,
        },
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {
            ProcessObservation::Absent
        }
        Err(_) => ProcessObservation::Unknown,
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{MacProcess, observe_macos_group, read_macos_process};

/// Includes zombies; denied, incomplete and invalid native reads remain unknown.
#[cfg(target_os = "macos")]
pub fn observe_process(pid: i32) -> ProcessObservation {
    if pid <= 0 {
        return ProcessObservation::Absent;
    }
    classify_macos_process(read_macos_process(pid))
}

#[cfg(target_os = "macos")]
fn classify_macos_process(process: std::io::Result<Option<MacProcess>>) -> ProcessObservation {
    match process {
        Ok(Some(process)) => ProcessObservation::Present {
            start_time: process.start_time,
            is_zombie: process.is_zombie,
        },
        Ok(None) => ProcessObservation::Absent,
        Err(_) => ProcessObservation::Unknown,
    }
}

#[cfg(all(test, target_os = "macos"))]
fn classify_bsd_info(
    pid: i32,
    written: i32,
    error: Option<i32>,
    info: &libc::proc_bsdinfo,
) -> ProcessObservation {
    classify_macos_process(macos::classify_bsd_info(pid, written, error, info))
}

/// Unsupported platforms cannot establish presence or absence.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn observe_process(_pid: i32) -> ProcessObservation {
    ProcessObservation::Unknown
}

/// Read the native start token, including from an unreaped zombie.
/// Absence and observation failures both return `None`; use [`observe_process`]
/// when deciding whether an old process is proven gone.
pub fn read_proc_start_time(pid: i32) -> Option<u64> {
    match observe_process(pid) {
        ProcessObservation::Present { start_time, .. } => Some(start_time),
        ProcessObservation::Absent | ProcessObservation::Unknown => None,
    }
}

/// Pure parser for `/proc/<pid>/stat` field 22 (`starttime`); cfg-gate-free so unit tests run on every host.
pub fn parse_starttime_from_stat(content: &str) -> Option<u64> {
    parse_proc_stat_fields(content).map(|fields| fields.start_time)
}

/// The identity-relevant subset of `/proc/<pid>/stat`: `state` (field 3), `pgrp` (field 5) and `starttime` (field 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStatFields {
    /// Single-char process state (`R`/`S`/`Z`/…) — field 3.
    pub state: char,
    /// Process-group id — field 5.
    pub pgrp: i32,
    /// `starttime` in clock-ticks since boot — field 22; the same value
    /// [`read_proc_start_time`] returns on Linux and owned-process verification checks.
    pub start_time: u64,
}

/// Pure parser for the [`ProcStatFields`] subset of `/proc/<pid>/stat`.
pub fn parse_proc_stat_fields(content: &str) -> Option<ProcStatFields> {
    // `comm` may contain `)` — split on the LAST `)`; the remainder starts with `state`.
    let after = content.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    // Post-comm indices: state(0) ppid(1) pgrp(2) … starttime is index 19.
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse::<i32>().ok()?;
    // `nth(16)` consumes 3..=19 and yields index 19 (`starttime`).
    let start_time = fields.nth(16)?.parse::<u64>().ok()?;
    Some(ProcStatFields {
        state,
        pgrp,
        start_time,
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let status = self.0.wait().expect("reap identity test child");
            eprintln!("identity test child {} reaped: {status}", self.0.id());
        }
    }

    #[test]
    fn proc_identity_live_child_is_stable_and_reaped_child_is_absent() {
        let mut child = ChildGuard(
            std::process::Command::new("sleep")
                .arg("60")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn identity test child"),
        );
        let pid = i32::try_from(child.0.id()).expect("pid fits i32");
        let start = read_proc_start_time(pid).expect("live child start time");
        assert_ne!(start, 0);
        assert_eq!(read_proc_start_time(pid), Some(start));
        assert_eq!(
            observe_process(pid),
            ProcessObservation::Present {
                start_time: start,
                is_zombie: false
            }
        );
        child.0.kill().expect("kill owned test child");
        child.0.wait().expect("reap owned test child");
        assert_eq!(read_proc_start_time(pid), None);
        assert_eq!(observe_process(pid), ProcessObservation::Absent);
    }

    #[test]
    fn proc_identity_invalid_pids_are_absent() {
        for pid in [i32::MIN, -1, 0, i32::MAX] {
            assert_eq!(read_proc_start_time(pid), None, "pid {pid}");
        }
    }
    #[test]
    fn proc_identity_denied_and_incomplete_stat_are_unknown() {
        for errno in [libc::EPERM, libc::EACCES, libc::EIO] {
            assert_eq!(
                classify_proc_stat(Err(std::io::Error::from_raw_os_error(errno))),
                ProcessObservation::Unknown
            );
        }
        for errno in [libc::ENOENT, libc::ESRCH] {
            assert_eq!(
                classify_proc_stat(Err(std::io::Error::from_raw_os_error(errno))),
                ProcessObservation::Absent
            );
        }
        assert_eq!(
            classify_proc_stat(Ok("123 (incomplete) S 1".into())),
            ProcessObservation::Unknown
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn proc_identity_bsd_denied_short_invalid_and_zombie_responses() {
        let pid = 123;
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        info.pbi_pid = pid as u32;
        info.pbi_start_tvsec = 1;
        for error in [None, Some(libc::EPERM), Some(libc::EACCES)] {
            assert_eq!(
                classify_bsd_info(pid, 0, error, &info),
                ProcessObservation::Unknown
            );
        }
        assert_eq!(
            classify_bsd_info(pid, 0, Some(libc::ESRCH), &info),
            ProcessObservation::Absent
        );
        assert_eq!(
            classify_bsd_info(pid, size - 1, Some(libc::ESRCH), &info),
            ProcessObservation::Unknown
        );
        assert_eq!(
            classify_bsd_info(pid + 1, size, None, &info),
            ProcessObservation::Unknown
        );
        info.pbi_status = libc::SZOMB;
        assert_eq!(
            classify_bsd_info(pid, size, None, &info),
            ProcessObservation::Present {
                start_time: 1_000_000,
                is_zombie: true
            }
        );
        info.pbi_start_tvusec = 1_000_000;
        assert_eq!(
            classify_bsd_info(pid, size, None, &info),
            ProcessObservation::Unknown
        );
        info.pbi_start_tvusec = 0;
        info.pbi_start_tvsec = 0;
        assert_eq!(
            classify_bsd_info(pid, size, None, &info),
            ProcessObservation::Unknown
        );
        info.pbi_start_tvsec = u64::MAX;
        assert_eq!(
            classify_bsd_info(pid, size, None, &info),
            ProcessObservation::Unknown
        );
    }
}
