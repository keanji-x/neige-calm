use super::*;
use std::time::{Duration, Instant};

struct Payload {
    process: Arc<Process>,
    output: Arc<super::super::output::OutputLog>,
    watchdog: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _root: tempfile::TempDir,
}
impl Payload {
    fn suspended(script: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let output =
            super::super::output::OutputLog::open(&root.path().join("output"), 4096).unwrap();
        let specification = ExecutionSpecification {
            version: calm_session::control::execution::SpecificationVersion::AmbientV2,
            ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            environment: vec![("PATH".into(), "/usr/bin:/bin".into())],
            io_mode: IoMode::Pipe,
            replay_bytes: 4096,
        };
        let (process, _) = spawn(
            &File::open(root.path()).unwrap(),
            &specification,
            CapabilityToken::fresh().unwrap(),
            output.clone(),
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let cleanup = process.clone();
        let thread = std::thread::spawn(move || {
            if receive.recv_timeout(Duration::from_secs(5)).is_err() {
                cleanup.cleanup().unwrap();
            }
        });
        Self {
            process,
            output,
            watchdog: Some(send),
            thread: Some(thread),
            _root: root,
        }
    }
    fn start(script: &str) -> Self {
        let payload = Self::suspended(script);
        payload.process.resume().unwrap();
        payload
    }
    fn exited(&self) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.process.poll().unwrap() {
                Observation::Exited {
                    status,
                    empty: true,
                } => {
                    self.process.reap().unwrap();
                    return status;
                }
                _ => assert!(
                    Instant::now() < deadline,
                    "owned native group failed to finish"
                ),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        let result = self.process.cleanup();
        let lost = matches!(
            self.process
                .ownership
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .child
                .pin,
            Pin::Lost
        );
        assert!(result.is_ok() || lost, "precise cleanup failed: {result:?}");
        if let Some(send) = self.watchdog.take() {
            let _ = send.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[tokio::test]
async fn native_macos_owned_spawn_exit_and_output() {
    for code in [0, 23] {
        let payload = Payload::start(&format!("printf native-output; exit {code}"));
        assert_eq!(payload.exited(), Some(code));
        assert!(matches!(
            payload.output.finish_bounded(Duration::from_secs(1)).await,
            OutputEnd::Complete { .. }
        ));
        assert_eq!(payload.output.snapshot(None).unwrap().1, b"native-output");
    }
}
#[test]
fn native_macos_lost_pin_never_signals() {
    let payload = Payload::suspended("exec sleep 30");
    {
        let mut owner = payload.process.ownership.lock().unwrap();
        owner.child.signal(false, libc::SIGKILL).unwrap();
        // Simulate an unexpected reap, still serialized under the sole owner.
        let mut status = 0;
        let rc = unsafe { libc::waitpid(owner.child.pid, &mut status, 0) };
        owner
            .child
            .record("external-reap", &format!("waitpid={rc} status={status}"));
        assert_eq!(rc, owner.child.pid);
        let before = owner.child.signals;
        assert!(owner.observe().is_err());
        assert!(matches!(owner.child.pin, Pin::Lost));
        assert!(owner.child.cleanup().is_err());
        assert_eq!(before, owner.child.signals);
    }
    assert!(payload.process.resume().is_err());
    assert!(payload.process.signal(ProcSignal::Kill).is_err());
}

#[test]
fn native_macos_latched_lost_pin_refuses_even_signal_zero() {
    let payload = Payload::suspended("exit 0");
    let (result, before, after) = {
        let mut owner = payload.process.ownership.lock().unwrap();
        let before = owner.child.signals;
        // Controlled loss of authority while the private, suspended child is still
        // owned. Signal zero cannot terminate anything, including under mutation.
        owner.child.pin = Pin::Lost;
        let result = owner.child.signal(false, 0);
        let after = owner.child.signals;
        // Restore our actual ownership before an assertion can unwind or watchdog fires.
        owner.child.pin = Pin::Owned;
        (result, before, after)
    };
    assert!(result.is_err(), "lost authority reached kill(2)");
    assert_eq!(before, after);
}
#[test]
fn native_macos_root_exits_before_same_group_and_unrelated_control_survives() {
    let control = Payload::start("exec sleep 30");
    let payload = Payload::start("sleep 30 & exit 17");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(
            payload.process.poll().unwrap(),
            Observation::Exited {
                status: Some(17),
                empty: false
            }
        ) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    payload.process.signal(ProcSignal::Term).unwrap();
    std::thread::sleep(STOP_GRACE);
    payload.process.signal(ProcSignal::Kill).unwrap();
    assert_eq!(payload.exited(), Some(17));
    assert!(matches!(
        control.process.poll().unwrap(),
        Observation::Running
    ));
}
#[test]
fn native_macos_term_resistant_requires_kill() {
    let payload = Payload::start("trap '' TERM; printf ready; exec sleep 30");
    let deadline = Instant::now() + Duration::from_secs(2);
    while payload.output.snapshot(None).unwrap().1 != b"ready" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    payload.process.signal(ProcSignal::Term).unwrap();
    std::thread::sleep(STOP_GRACE);
    assert!(matches!(
        payload.process.poll().unwrap(),
        Observation::Running
    ));
    payload.process.signal(ProcSignal::Kill).unwrap();
    assert_eq!(payload.exited(), None);
}

#[test]
fn native_macos_cleanup_suspended_panic_and_timeout() {
    let mut pid = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let payload = Payload::suspended("exec sleep 30");
        pid = payload.process.ownership.lock().unwrap().identity.pid as i32;
        let raw = payload
            .process
            .ownership
            .lock()
            .unwrap()
            .child
            .wait_info()
            .unwrap();
        eprintln!(
            "RAW waitid rc=0 errno=0 si_signo={} si_pid={} si_code={} si_status={}",
            raw.si_signo, raw.si_pid, raw.si_code, raw.si_status
        );
        panic!("injected before resume");
    }));
    assert!(result.is_err());
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let payload = Payload::suspended("exec sleep 30");
    std::thread::sleep(Duration::from_secs(6));
    assert!(matches!(
        payload.process.ownership.lock().unwrap().child.pin,
        Pin::Reaped
    ));
}

#[test]
fn native_macos_suspended_wait_event_allows_resume() {
    let payload = Payload::suspended("exec sleep 30");
    let result = payload.process.resume();
    assert!(result.is_ok(), "suspended resume: {result:?}");
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Fault {
    None,
    Setup,
    Observe,
    Group,
}
thread_local! { static FAULT: std::cell::Cell<Fault> = const { std::cell::Cell::new(Fault::None) }; }
pub(super) fn fault(point: Fault) -> anyhow::Result<()> {
    anyhow::ensure!(FAULT.get() != point, "injected native read/setup failure");
    Ok(())
}
struct FaultGuard;
impl FaultGuard {
    fn set(fault: Fault) -> Self {
        FAULT.set(fault);
        Self
    }
}
impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAULT.set(Fault::None);
    }
}

#[test]
fn native_macos_cleanup_setup_and_observation_failures() {
    {
        let _fault = FaultGuard::set(Fault::Setup);
        assert!(std::panic::catch_unwind(|| Payload::suspended("exec sleep 30")).is_err());
    }
    let payload = Payload::suspended("exec sleep 30");
    {
        let _fault = FaultGuard::set(Fault::Observe);
        assert!(payload.process.resume().is_err());
        assert!(payload.process.poll().is_err());
        assert!(matches!(
            payload.process.ownership.lock().unwrap().child.pin,
            Pin::Owned
        ));
        payload.process.cleanup().unwrap();
    }
    assert!(matches!(
        payload.process.ownership.lock().unwrap().child.pin,
        Pin::Reaped
    ));
    let before = payload.process.ownership.lock().unwrap().child.signals;
    assert!(payload.process.resume().is_err());
    payload.process.signal(ProcSignal::Kill).unwrap();
    assert_eq!(
        before,
        payload.process.ownership.lock().unwrap().child.signals
    );
}

#[test]
fn native_macos_group_read_failure_retains_cleanup_pin() {
    let payload = Payload::start("exec sleep 30");
    let _fault = FaultGuard::set(Fault::Group);
    assert!(payload.process.signal(ProcSignal::Kill).is_err());
    assert!(matches!(
        payload.process.ownership.lock().unwrap().child.pin,
        Pin::Owned
    ));
    payload.process.cleanup().unwrap();
    assert!(matches!(
        payload.process.ownership.lock().unwrap().child.pin,
        Pin::Reaped
    ));
}

#[test]
fn native_macos_wait_events_are_not_invented_exit_facts() {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    assert_eq!(wait_status(&info, 42).unwrap(), None);
    info.si_signo = libc::SIGCHLD;
    info.si_pid = 42;
    for code in [libc::CLD_STOPPED, libc::CLD_CONTINUED] {
        info.si_code = code;
        assert_eq!(wait_status(&info, 42).unwrap(), None);
    }
    info.si_code = 999;
    assert!(wait_status(&info, 42).is_err());
    info.si_code = libc::CLD_EXITED;
    info.si_status = 23;
    assert_eq!(wait_status(&info, 42).unwrap(), Some(Some(23)));
    info.si_pid = 43;
    assert!(wait_status(&info, 42).is_err());
    for code in [libc::CLD_KILLED, libc::CLD_DUMPED] {
        info.si_pid = 42;
        info.si_code = code;
        assert_eq!(wait_status(&info, 42).unwrap(), Some(None));
    }
}
