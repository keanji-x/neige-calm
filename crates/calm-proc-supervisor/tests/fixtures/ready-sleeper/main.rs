use std::io::Write;
use std::os::fd::FromRawFd;
use std::time::Duration;

fn main() {
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
