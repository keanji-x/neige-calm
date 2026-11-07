//! Read-only restart evidence. A stored PID never authorizes a signal or reap.
use workspace::execution::{MacProcessIdentity, NativeProcessIdentity, ScopeMechanismIdentity};

pub(super) fn quiescent(
    process: &NativeProcessIdentity,
    mechanism: &ScopeMechanismIdentity,
) -> bool {
    let (
        NativeProcessIdentity::MacOwnedGroup(identity),
        ScopeMechanismIdentity::MacOwnedGroup {
            protocol_version: 1,
        },
    ) = (process, mechanism)
    else {
        return false;
    };
    if identity.pid == 0
        || identity.pid > i32::MAX as u32
        || identity.birth_seconds == 0
        || identity.birth_microseconds >= 1_000_000
    {
        return false;
    }
    observe(identity, process_info, group_empty)
}

fn observe(
    identity: &MacProcessIdentity,
    process: impl FnOnce(u32) -> (i32, i32, libc::proc_bsdinfo),
    group: impl FnOnce(u32) -> (i32, i32),
) -> bool {
    let (count, errno, info) = process(identity.pid);
    let absent = if count == std::mem::size_of::<libc::proc_bsdinfo>() as i32 && errno == 0 {
        info.pbi_pid == identity.pid
            && info.pbi_start_tvsec > 0
            && info.pbi_start_tvusec < 1_000_000
            && (info.pbi_start_tvsec != identity.birth_seconds
                || info.pbi_start_tvusec != identity.birth_microseconds)
    } else {
        count == 0 && errno == libc::ESRCH
    };
    if !absent {
        return false;
    }
    let (bytes, errno) = group(identity.pid);
    // libproc maps syscall errors to zero too. Any nonzero result (including
    // a full/truncated buffer) means we have not established an empty group.
    bytes == 0 && errno == 0
}

fn process_info(pid: u32) -> (i32, i32, libc::proc_bsdinfo) {
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let (count, errno) = unsafe {
        *libc::__error() = 0;
        // arg=1 includes zombies; the original identity must be gone even if
        // it moved out of the original group before its owner was interrupted.
        let count = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            std::mem::size_of_val(&info) as i32,
        );
        (count, *libc::__error())
    };
    (count, errno, info)
}
fn group_empty(pgid: u32) -> (i32, i32) {
    let mut pid = 0i32;
    unsafe {
        *libc::__error() = 0;
        // sys/proc_info.h: PROC_PGRP_ONLY = 2. XNU scans allproc and zombproc
        // under proc_list_lock. A non-null one-PID buffer suffices for emptiness;
        // a null buffer would instead return an estimated capacity.
        let bytes = libc::proc_listpids(
            2,
            pgid,
            (&mut pid as *mut i32).cast(),
            std::mem::size_of::<i32>() as i32,
        );
        (bytes, *libc::__error())
    }
}

#[cfg(test)]
#[path = "native_recovery_macos_tests.rs"]
mod tests;
