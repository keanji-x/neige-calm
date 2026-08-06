use std::io::Write;

const SELF_TERMINATE_SECS: libc::c_uint = 120;

fn main() {
    // Bound leaks even if the test process is aborted before its exact-pid
    // cleanup guard runs. portable-pty's pre_exec resets SIGALRM to SIG_DFL
    // and clears the signal mask (unix.rs:241-253).
    // This does not cover a stopped process, which cannot act on SIGALRM;
    // openpty leaves TOSTOP clear and this fixture never enables it.
    unsafe { libc::alarm(SELF_TERMINATE_SECS) };

    let mut sync_fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(sync_fds.as_mut_ptr()) }, 0, "pipe");

    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork");
    if child == 0 {
        unsafe {
            // alarm(2) state is not inherited across fork, so arm the detached
            // foreground job independently of the fixture's leader.
            libc::alarm(SELF_TERMINATE_SECS);
            libc::close(sync_fds[0]);
            assert_eq!(libc::setpgid(0, 0), 0, "setpgid");
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            let byte = [1_u8];
            assert_eq!(libc::write(sync_fds[1], byte.as_ptr().cast(), 1), 1);
            libc::close(sync_fds[1]);
            loop {
                libc::pause();
            }
        }
    }

    unsafe {
        libc::close(sync_fds[1]);
        let mut byte = [0_u8];
        assert_eq!(libc::read(sync_fds[0], byte.as_mut_ptr().cast(), 1), 1);
        libc::close(sync_fds[0]);
        assert_eq!(libc::tcsetpgrp(libc::STDIN_FILENO, child), 0, "tcsetpgrp");
    }
    println!("foreground_pid={child}");
    std::io::stdout().flush().expect("flush foreground pid");
    loop {
        unsafe { libc::pause() };
    }
}
