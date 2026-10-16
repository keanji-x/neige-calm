#[cfg(target_os = "linux")]
mod bind_alias;
#[cfg(target_os = "linux")]
mod qualification_probe;

use std::io::Write;

const SELF_TERMINATE_SECS: libc::c_uint = 120;

fn main() {
    #[cfg(target_os = "linux")]
    {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        if arguments
            .get(1)
            .is_some_and(|argument| argument == "--execution-helper")
        {
            // Fault image for the read-only observer's actual timeout/reap path. No fork, protocol
            // implementation, environment switch, or report is provided by this fixture role.
            assert_eq!(arguments.len(), 4);
            loop {
                unsafe {
                    libc::pause();
                }
            }
        }
        if arguments
            .get(1)
            .is_some_and(|argument| argument == "--probe-bind-alias")
        {
            assert_eq!(
                arguments.len(),
                4,
                "expected owned root and alias directories"
            );
            bind_alias::run(
                std::path::Path::new(&arguments[2]),
                std::path::Path::new(&arguments[3]),
            )
            .expect("controlled bind alias capability probe");
            return;
        }
        if arguments
            .get(1)
            .is_some_and(|argument| argument == "--probe-object-qualification")
        {
            assert_eq!(
                arguments.len(),
                5,
                "expected owned root, source and actual executor helper"
            );
            qualification_probe::run(
                std::path::Path::new(&arguments[2]),
                std::path::Path::new(&arguments[3]),
                std::path::Path::new(&arguments[4]),
            )
            .expect("controlled object qualification probe");
            return;
        }
    }
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
