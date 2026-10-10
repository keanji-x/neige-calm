//! Test fixture: a process that owns one browser, so a test can SIGKILL the
//! owner or read the browser's environment.
//!
//! Usage: `chrome-control-test-owner <binary> <profile> <home> <display> <runtime-dir>`
//!
//! Before launching it opens `<profile>/owner-inherited-marker` without
//! close-on-exec on fd 50 or above, as a careless host process might. It launches through
//! `Chrome::launch`, prints `browser <pid>`, then waits. A `stop` line on stdin
//! stops the browser and prints `stopped <status>`; stdin EOF drops the handle
//! (killing the browser's group) and exits.
#[cfg(target_os = "linux")]
fn main() {
    owner::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod owner {
    use std::io::Write;

    use chrome_control::{Chrome, LaunchConfig, WaylandEnv};

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let [binary, profile, home, display, runtime_dir] = args.as_slice() else {
            panic!("usage: <binary> <profile> <home> <display> <runtime-dir>");
        };
        let config = LaunchConfig {
            binary: binary.into(),
            profile_dir: profile.into(),
            home_dir: home.into(),
            wayland: WaylandEnv {
                display: display.into(),
                runtime_dir: runtime_dir.into(),
            },
            size: (800, 600),
        };
        let marker = std::path::Path::new(profile).join("owner-inherited-marker");
        std::fs::write(&marker, b"").unwrap();
        let marker = std::ffi::CString::new(marker.into_os_string().into_encoded_bytes()).unwrap();
        // Kept at fd 50 or above, away from the 3 and 4 that the CDP pipes take
        // over in the browser. SAFETY: plain fd calls; the descriptor stays
        // open, without close-on-exec, for the owner's lifetime.
        unsafe {
            let fd = libc::open(marker.as_ptr(), libc::O_RDONLY);
            assert!(fd >= 0 && libc::fcntl(fd, libc::F_DUPFD, 50) >= 50);
            libc::close(fd);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let chrome = match runtime.block_on(Chrome::launch(config)) {
            Ok(chrome) => chrome,
            Err(error) => {
                println!("error {error}");
                std::process::exit(1);
            }
        };
        println!("browser {}", chrome.pid());
        std::io::stdout().flush().unwrap();
        for line in std::io::stdin().lines() {
            if line.as_deref().is_ok_and(|line| line == "stop") {
                let status = runtime.block_on(chrome.stop());
                println!("stopped {status:?}");
                return;
            }
        }
        // stdin closed: the test that started us is gone. Returning drops the
        // handle, which kills the browser's group.
    }
}
