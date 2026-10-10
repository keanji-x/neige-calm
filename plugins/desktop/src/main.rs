//! The `desktop` plugin binary; see the library for what it does.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    desktop::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("the desktop plugin runs on Linux only");
    std::process::ExitCode::FAILURE
}
