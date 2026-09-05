#[cfg(target_os = "linux")]
use std::io::{BufRead, Write};
#[cfg(target_os = "linux")]
fn main() {
    std::fs::write("started", "yes").unwrap();
    if std::env::args().any(|arg| arg == "writer") {
        unsafe {
            let child = libc::fork();
            assert!(child >= 0);
            if child == 0 {
                assert!(libc::setsid() > 0);
                let grandchild = libc::fork();
                assert!(grandchild >= 0);
                if grandchild != 0 {
                    libc::_exit(0);
                }
                loop {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("beats")
                        .unwrap();
                    file.write_all(b"x").unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line == "close-stdio" {
            std::fs::write("stdio-closed", "yes").unwrap();
            unsafe {
                libc::close(0);
                libc::close(1);
            }
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        if line == "final-exit" {
            println!("final response");
            std::io::stdout().flush().unwrap();
            break;
        }
        if line == "exit" {
            break;
        }
        if let Some(name) = line.strip_prefix("env ") {
            println!(
                "{}",
                std::env::var(name).unwrap_or_else(|_| "absent".into())
            );
        } else if let Some(path) = line.strip_prefix("exists ") {
            println!("{}", std::path::Path::new(path).exists());
        } else {
            println!("{line}");
        }
        std::io::stdout().flush().unwrap();
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("this test provider requires Linux");
}
