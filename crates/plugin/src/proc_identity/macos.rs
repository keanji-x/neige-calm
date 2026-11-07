//! Darwin observations only: these values do not grant signal authority.
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacProcess {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: u64,
    pub is_zombie: bool,
}

/// Includes zombies. Only ESRCH proves absence; denied/incomplete observations are errors.
pub fn read_macos_process(pid: i32) -> io::Result<Option<MacProcess>> {
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "positive PID required",
        ));
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: info is valid writable storage of exactly size bytes.
    let written = unsafe {
        *libc::__error() = 0;
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            1,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    let error = (written == 0)
        .then(io::Error::last_os_error)
        .and_then(|e| e.raw_os_error());
    classify_bsd_info(pid, written, error, &info)
}

pub(super) fn classify_bsd_info(
    pid: i32,
    written: i32,
    error: Option<i32>,
    info: &libc::proc_bsdinfo,
) -> io::Result<Option<MacProcess>> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    if written != size {
        return if written == 0 && error == Some(libc::ESRCH) {
            Ok(None)
        } else if let Some(errno) = error.filter(|errno| *errno != 0) {
            Err(io::Error::from_raw_os_error(errno))
        } else {
            Err(io::Error::other("incomplete native process observation"))
        };
    }
    let start_time = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
        .filter(|start| *start != 0);
    if info.pbi_pid != pid as u32 || info.pbi_start_tvusec >= 1_000_000 || start_time.is_none() {
        return Err(io::Error::other("invalid native process identity"));
    }
    Ok(Some(MacProcess {
        pid,
        pgid: i32::try_from(info.pbi_pgid).map_err(io::Error::other)?,
        start_time: start_time.expect("validated start identity"),
        is_zombie: info.pbi_status == libc::SZOMB,
    }))
}

/// Observe members, including zombies, without granting signal authority.
/// An empty/all-zombie result proves no live members at one complete enumeration.
/// Disappearing members, changed identities and incomplete observations are errors.
/// Callers must establish ownership separately and recheck each identity before signaling.
pub fn observe_macos_group(pgid: i32) -> io::Result<Vec<MacProcess>> {
    observe_group(
        pgid,
        #[cfg(test)]
        &mut |_| {},
    )
}

fn observe_group(
    pgid: i32,
    #[cfg(test)] after_list: &mut impl FnMut(&[i32]),
) -> io::Result<Vec<MacProcess>> {
    let pids = list_group(
        pgid,
        #[cfg(test)]
        after_list,
    )?;
    let mut members = Vec::with_capacity(pids.len());
    for pid in &pids {
        let member = read_macos_process(*pid)?
            .ok_or_else(|| io::Error::other("native group member disappeared"))?;
        if member.pgid != pgid {
            return Err(io::Error::other("native group changed during observation"));
        }
        members.push(member);
    }
    if !members.is_empty() && members.iter().all(|m| m.is_zombie) {
        // The first list may predate a fork/join followed by the last member's exit.
        // Anchor absence at a new complete list, bracketed by reads of the SAME
        // zombie identities. A zombie cannot revive or fork; matching births after
        // the list exclude PID recycling. Thus every member was dead at that list.
        // This is a terminal-state proof, not an assumption that two scans suffice.
        let confirmed = list_group(
            pgid,
            #[cfg(test)]
            after_list,
        )?;
        if confirmed != pids {
            return Err(io::Error::other("native group changed during empty proof"));
        }
        for member in &members {
            if read_macos_process(member.pid)? != Some(*member) {
                return Err(io::Error::other(
                    "native zombie identity changed during empty proof",
                ));
            }
        }
    }
    Ok(members)
}

fn list_group(pgid: i32, #[cfg(test)] after_list: &mut impl FnMut(&[i32])) -> io::Result<Vec<i32>> {
    if pgid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "safe PGID required",
        ));
    }
    let mut capacity = 32;
    while capacity <= 65_536 {
        let mut pids = vec![0i32; capacity];
        // SAFETY: the buffer holds capacity PIDs. Darwin returns a PID count, not bytes.
        let count = unsafe {
            *libc::__error() = 0;
            libc::proc_listpgrppids(
                pgid,
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        let error = io::Error::last_os_error();
        if count < 0 || (count == 0 && error.raw_os_error() != Some(0)) {
            return Err(error);
        }
        if count as usize >= capacity {
            capacity *= 2;
            continue;
        }
        pids.truncate(count as usize);
        #[cfg(test)]
        after_list(&pids);
        pids.sort_unstable();
        return Ok(pids);
    }
    Err(io::Error::other(
        "native group observation capacity exhausted",
    ))
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
