use std::io::Write;
use std::os::fd::FromRawFd;
use std::time::Duration;

fn main() {
    let bootstrap = std::env::args().collect::<Vec<_>>();
    if bootstrap.get(1).map(String::as_str) == Some("--execution-helper") {
        // Fault image: consume the real bootstrap before failing, so EPIPE vs child-exit
        // scheduling cannot hide the observer's concrete failure/reap assertion.
        assert_eq!(bootstrap.len(), 4);
        let fd = bootstrap[2].parse::<i32>().unwrap();
        assert!(fd > 2);
        let mut configuration = unsafe { std::fs::File::from_raw_fd(fd) };
        std::io::copy(&mut configuration, &mut std::io::sink()).unwrap();
        std::process::exit(23);
    }
    let mut args = std::env::args().skip(1);
    let mut ready_fd = None;
    while let Some(arg) = args.next() {
        if arg == "--ready-fd" {
            ready_fd = args.next().and_then(|value| value.parse::<i32>().ok());
        }
    }
    let fd = ready_fd.expect("--ready-fd");
    unsafe {
        let mut file = std::fs::File::from_raw_fd(fd);
        file.write_all(b"ready\n").expect("write ready");
        file.flush().expect("flush ready");
    }
    std::thread::sleep(Duration::from_secs(30));
}
