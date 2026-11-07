use super::*;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

// A real exec witness, not another implementation of the app-server protocol.
// The SQLite writer barrier below keeps the failure cleanup from racing its
// first instruction. Each PID is captured while that barrier is still held.
pub(super) struct ExecWitness {
    pub(super) binary: PathBuf,
    ledger: PathBuf,
    pub(super) prefix: PathBuf,
    owned: std::sync::Mutex<Vec<OwnedPid>>,
}

struct OwnedPid {
    pid: i32,
    birth: u64,
    boot: String,
}

impl Drop for OwnedPid {
    fn drop(&mut self) {
        if read_proc_start_time(self.pid) == Some(self.birth)
            && read_boot_id().as_ref() == Some(&self.boot)
            && unsafe { libc::getpgid(self.pid) } == self.pid
        {
            // Only this fixture's captured direct child; no group-wide cleanup.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

pub(super) fn optional_text(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

impl ExecWitness {
    pub(super) fn new(root: &Path) -> Self {
        let witness = Self {
            binary: root.join("counted-codex"),
            ledger: root.join("exec-pids"),
            prefix: root.join("spawn"),
            owned: Default::default(),
        };
        std::fs::write(
            &witness.binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> {}\nexport FAKE_CODEX_SIGTERM_MARKER={}.term.\"$$\"\nexport FAKE_CODEX_HANDLER_READY_MARKER={}.ready.\"$$\"\nexec {} \"$@\"\n",
                shell_quote(&witness.ledger),
                shell_quote(&witness.prefix),
                shell_quote(&witness.prefix),
                shell_quote(Path::new(fake_codex_bin())),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&witness.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        witness
    }

    pub(super) fn pids(&self) -> Vec<i32> {
        optional_text(&self.ledger)
            .lines()
            .map(|line| line.parse().expect("exec witness PID"))
            .collect()
    }

    // Called after releasing the owner so its watcher cannot restart a reaped child.
    // Reuse the captured birth/boot fence and report any matching process left behind.
    pub(super) fn reap_captured(&self) -> Vec<i32> {
        let children = std::mem::take(&mut *self.owned.lock().unwrap());
        let identities: Vec<_> = children
            .iter()
            .map(|child| (child.pid, child.birth))
            .collect();
        drop(children);
        identities
            .into_iter()
            .filter_map(|(pid, birth)| (read_proc_start_time(pid) == Some(birth)).then_some(pid))
            .collect()
    }

    pub(super) async fn capture_new_exec(&self, previous: usize) {
        loop {
            let pids = self.pids();
            if let Some(&pid) = pids.get(previous)
                && let Some(birth) = read_proc_start_time(pid)
            {
                let mut owned = self.owned.lock().unwrap();
                if !owned.iter().any(|child| child.pid == pid) {
                    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
                    owned.push(OwnedPid {
                        pid,
                        birth,
                        boot: read_boot_id().expect("real process boot identity"),
                    });
                }
                if PathBuf::from(format!("{}.ready.{pid}", self.prefix.display())).exists() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

pub(super) struct LiveEndpoint {
    child: std::process::Child,
    pub(super) pid: i32,
    pub(super) birth: u64,
    pub(super) boot: String,
    marker: PathBuf,
    socket: PathBuf,
    inode: (u64, u64),
    methods: String,
}

impl LiveEndpoint {
    pub(super) async fn new(root: &Path, socket: &Path) -> Self {
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let marker = root.join("independent-term");
        let ready = root.join("independent-ready");
        let boot = read_boot_id().expect("real process boot identity");
        let child = std::process::Command::new(fake_codex_bin())
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", socket.display()))
            .env("FAKE_CODEX_SIGTERM_MARKER", &marker)
            .env("FAKE_CODEX_HANDLER_READY_MARKER", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        // Install cleanup immediately, including on a readiness assertion failure.
        let mut live = Self {
            pid: child.id() as i32,
            child,
            birth: 0,
            boot,
            marker,
            socket: socket.to_owned(),
            inode: (0, 0),
            methods: String::new(),
        };
        live.birth = wait_for_start_time_and_socket(live.pid, socket).await;
        assert!(
            ready.exists(),
            "TERM handler armed before endpoint publication"
        );
        assert!(live.child.try_wait().unwrap().is_none());
        let metadata = std::fs::metadata(socket).unwrap();
        live.inode = (metadata.dev(), metadata.ino());
        live.methods = optional_text(&socket.with_extension("methods"));
        assert!(tokio::net::UnixStream::connect(socket).await.is_ok());
        live
    }

    pub(super) async fn effects(&mut self) -> EndpointEffects {
        let inode = std::fs::symlink_metadata(&self.socket)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()));
        EndpointEffects {
            alive: self.child.try_wait().unwrap().is_none()
                && read_proc_start_time(self.pid) == Some(self.birth)
                && read_boot_id().as_ref() == Some(&self.boot),
            no_term: !self.marker.exists(),
            same_inode: inode == Some(self.inode),
            connectable: tokio::time::timeout(
                Duration::from_secs(1),
                tokio::net::UnixStream::connect(&self.socket),
            )
            .await
            .is_ok_and(|result| result.is_ok()),
            no_requests: optional_text(&self.socket.with_extension("methods")) == self.methods,
        }
    }
}

impl Drop for LiveEndpoint {
    fn drop(&mut self) {
        let _ = self.child.kill();
        self.child.wait().expect("reap independent fixture child");
    }
}

#[derive(Debug)]
pub(super) struct EndpointEffects {
    pub(super) alive: bool,
    pub(super) no_term: bool,
    pub(super) same_inode: bool,
    pub(super) connectable: bool,
    pub(super) no_requests: bool,
}
