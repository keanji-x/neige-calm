use super::*;

fn identity() -> MacProcessIdentity {
    MacProcessIdentity {
        pid: 42,
        birth_seconds: 100,
        birth_microseconds: 10,
        nonce: workspace::execution::CapabilityToken::fresh().unwrap(),
    }
}
fn info(pid: u32, birth: u64) -> libc::proc_bsdinfo {
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    info.pbi_pid = pid;
    info.pbi_start_tvsec = birth;
    info.pbi_start_tvusec = 10;
    info
}
#[test]
fn recovery_3450_identity_errors_and_truncation_are_unknown() {
    for (count, errno) in [
        (0, 0),
        (0, libc::EPERM),
        (0, libc::EACCES),
        (-1, libc::ESRCH),
        (1, 0),
        (1, libc::ESRCH),
    ] {
        assert!(!observe(
            &identity(),
            |_| (count, errno, info(42, 200)),
            |_| (0, 0)
        ));
    }
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    for (pid, birth, errno) in [
        (43, 200, 0),
        (42, 0, 0),
        (42, 200, libc::EPERM),
        (42, 100, 0),
    ] {
        assert!(!observe(
            &identity(),
            |_| (size, errno, info(pid, birth)),
            |_| (0, 0)
        ));
    }
}
#[test]
fn recovery_3450_group_errors_and_members_are_unknown() {
    for result in [
        (0, libc::EPERM),
        (0, libc::EACCES),
        (0, libc::ESRCH),
        (-1, 0),
        (1, 0),
        (4, 0),
        (8, 0),
    ] {
        assert!(!observe(
            &identity(),
            |_| (0, libc::ESRCH, info(0, 0)),
            |_| result
        ));
    }
}
#[test]
fn recovery_3450_only_missing_or_reused_identity_and_empty_group_qualify() {
    assert!(observe(
        &identity(),
        |_| (0, libc::ESRCH, info(0, 0)),
        |_| (0, 0)
    ));
    assert!(observe(
        &identity(),
        |_| (
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
            0,
            info(42, 200)
        ),
        |_| (0, 0)
    ));
}
#[test]
fn recovery_3450_native_live_identity_and_reaped_group() {
    let (count, errno, current) = process_info(std::process::id());
    assert_eq!(count, std::mem::size_of_val(&current) as i32);
    assert_eq!(errno, 0);
    let original = MacProcessIdentity {
        pid: current.pbi_pid,
        birth_seconds: current.pbi_start_tvsec,
        birth_microseconds: current.pbi_start_tvusec,
        ..identity()
    };
    assert!(!observe(&original, process_info, |_| (0, 0)));
    assert!(group_empty(current.pbi_pgid).0 > 0);
}
