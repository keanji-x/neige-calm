//! Narrow Darwin process observations. No lifecycle or signalling authority is granted.
use std::io;

pub fn process_info(pid: u32) -> io::Result<libc::proc_bsdinfo> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid native pid",
        ));
    }
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
    if count != std::mem::size_of_val(&info) as i32 {
        let error = io::Error::last_os_error();
        return Err(
            if count == 0 && error.raw_os_error().is_some_and(|e| e != 0) {
                error
            } else {
                io::Error::other("incomplete native process information")
            },
        );
    }
    if info.pbi_pid != pid {
        return Err(io::Error::other("native process identity changed"));
    }
    Ok(info)
}
/// Snapshot of the original group, including its retained leader and zombies.
/// These are OS facts only: callers must hold and validate their own child pin.
pub fn group_members(leader: &libc::proc_bsdinfo) -> io::Result<Vec<libc::proc_bsdinfo>> {
    let pid = leader.pbi_pid;
    if pid == 0 || pid > i32::MAX as u32 || leader.pbi_pgid != pid {
        return Err(io::Error::other("invalid native group leader"));
    }
    let pids = group_pids(pid)?;
    let mut members = Vec::new();
    for member in &pids {
        let info = process_info(*member as u32)?;
        if info.pbi_pgid != pid {
            return Err(io::Error::other("native group changed during observation"));
        }
        if *member as u32 == pid
            && (info.pbi_start_tvsec != leader.pbi_start_tvsec
                || info.pbi_start_tvusec != leader.pbi_start_tvusec)
        {
            return Err(io::Error::other("native leader birth identity changed"));
        }
        members.push(info);
    }
    // A member could fork after the PID list and exit before its status query.
    // Once every observed member is a zombie, none can fork again: a matching
    // fresh list rules out an unobserved live child from that interval.
    if members
        .iter()
        .all(|member| member.pbi_status == libc::SZOMB)
        && group_pids(pid)? != pids
    {
        return Err(io::Error::other("native group changed during observation"));
    }
    Ok(members)
}

fn group_pids(pid: u32) -> io::Result<Vec<i32>> {
    let mut capacity = 32;
    loop {
        if capacity > 65536 {
            return Err(io::Error::other(
                "native group observation capacity exhausted",
            ));
        }
        let mut pids = vec![0i32; capacity];
        unsafe {
            *libc::__error() = 0;
        }
        // Darwin returns a PID count; its buffer size argument is in bytes.
        let count = unsafe {
            libc::proc_listpgrppids(
                pid as i32,
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        if count <= 0 {
            return Err(io::Error::other(format!(
                "native group observation failed/empty: {}",
                io::Error::last_os_error()
            )));
        }
        if count as usize >= capacity {
            capacity *= 2;
            continue;
        }
        pids.truncate(count as usize);
        if !pids.contains(&(pid as i32)) {
            return Err(io::Error::other(
                "native group observation lost pinned leader",
            ));
        }
        if pids.iter().any(|member| *member <= 0) {
            return Err(io::Error::other("invalid native group member"));
        }
        pids.sort_unstable();
        return Ok(pids);
    }
}
