//! Test fixture: a process that owns one browser, so a test can SIGKILL the
//! owner or read the browser's environment.
//!
//! Usage: `chrome-control-test-owner <binary> <profile> <home> <display> <runtime-dir>`
//!
//! Launches through `Chrome::launch`, prints `browser <pid>`, then waits. A
//! `stop` line on stdin stops the browser and prints `stopped <status>`.
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
        // stdin closed: keep owning the browser until killed.
        loop {
            std::thread::park();
        }
    }
}
