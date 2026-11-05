//! Test supervisor pin: timeout, shutdown and reap share this one Child handle.
use std::{
    io::{self, Write},
    process::{Child, ExitStatus},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

pub(crate) struct SupervisorChild {
    child: Arc<Mutex<Child>>,
    cancel: Option<mpsc::Sender<()>>,
    watchdog: Option<std::thread::JoinHandle<io::Result<ExitStatus>>>,
}
impl SupervisorChild {
    pub fn new(child: Child, root: &std::path::Path) -> Self {
        Self::with_timeout(child, root, Duration::from_secs(20))
    }
    pub fn with_timeout(child: Child, root: &std::path::Path, budget: Duration) -> Self {
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let (cancel, rx) = mpsc::channel();
        let owned = child.clone();
        // Arm cleanup before any fallible identity inspection or test assertion.
        let watchdog = std::thread::spawn(move || {
            let _ = rx.recv_timeout(budget);
            shutdown(&owned)
        });
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                std::mem::size_of_val(&info) as i32,
            )
        };
        record(&format!(
            "supervisor pid={pid} birth={}.{} info_rc={rc} root={}",
            info.pbi_start_tvsec,
            info.pbi_start_tvusec,
            root.display()
        ));
        Self {
            child,
            cancel: Some(cancel),
            watchdog: Some(watchdog),
        }
    }
    pub fn id(&self) -> u32 {
        self.child.lock().unwrap().id()
    }
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.lock().unwrap().kill()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.lock().unwrap().try_wait()
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn close(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            let result = watchdog.join().expect("supervisor watchdog panicked");
            assert!(
                result.is_ok(),
                "supervisor child cleanup failed: {result:?}"
            );
        }
    }
}
impl Drop for SupervisorChild {
    fn drop(&mut self) {
        self.close();
    }
}
fn record(detail: &str) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "native-test-ledger {detail}");
    let _ = stderr.flush();
}
fn shutdown(owned: &Mutex<Child>) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut sent = false;
    loop {
        {
            let mut child = owned.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(status) = child.try_wait()? {
                record(&format!(
                    "supervisor pid={} reaped status={status}",
                    child.id()
                ));
                return Ok(status);
            }
            if !sent {
                // try_wait retained a live child under this same mutex.
                let rc = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                record(&format!("supervisor pid={} SIGTERM rc={rc}", child.id()));
                if rc != 0 {
                    return Err(io::Error::last_os_error());
                }
                sent = true;
            }
        }
        if Instant::now() >= deadline {
            // Killing the parent here would discard its payload pins. Stop this
            // test binary instead of starting more tests with unknown children.
            record("FATAL supervisor failed bounded owned-child shutdown; no new spawn permitted");
            std::process::abort();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_macos_supervisor_pin_cleanup_on_panic_and_timeout() {
    let root = tempfile::tempdir().unwrap();
    let mut pid = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let owned = SupervisorChild::new(child, root.path());
        pid = owned.id();
        panic!("test supervisor fixture unwind");
    }));
    assert!(result.is_err());
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut owned = SupervisorChild::with_timeout(child, root.path(), Duration::from_millis(20));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            Instant::now() < deadline,
            "supervisor watchdog did not finish within the observation budget"
        );
        if owned.watchdog.as_ref().unwrap().is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Check the watchdog's reap before try_wait could reap on its behalf.
    assert_eq!(
        unsafe { libc::waitpid(owned.id() as i32, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(owned.try_wait().unwrap().is_some());
}
