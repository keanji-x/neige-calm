use super::*;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

const CHILD: &str = "operation::workspace_lease::metadata_lock::tests::metadata_lock_child";

thread_local! {
    static CONTENTION_SIGNAL: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn observe_contention() {
    CONTENTION_SIGNAL.with(|signal| {
        if let Some(path) = signal.borrow_mut().take() {
            std::fs::write(path, "contended").unwrap();
        }
    });
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child(repo: &Path, ready: &Path, exec: bool) -> ChildGuard {
    ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--nocapture"])
            .env("NEIGE_METADATA_TEST_REPO", repo)
            .env("NEIGE_METADATA_TEST_READY", ready)
            .env("NEIGE_METADATA_TEST_EXEC", if exec { "1" } else { "0" })
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn wait_ready(child: &mut ChildGuard, ready: &Path) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before acquiring lock"
        );
        assert!(Instant::now() < until, "child did not acquire lock");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn bare_repo(path: &Path) {
    assert!(
        crate::workspace_materialize::neige_git_command()
            .args(["init", "--bare", "-q"])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn metadata_lock_child() {
    let Some(repo) = std::env::var_os("NEIGE_METADATA_TEST_REPO") else {
        return;
    };
    let ready = std::env::var_os("NEIGE_METADATA_TEST_READY").unwrap();
    let ready = Path::new(&ready);
    CONTENTION_SIGNAL.with(|signal| {
        *signal.borrow_mut() = Some(ready.with_extension("contended"));
    });
    let _lock = GitMetadataLock::acquire(Path::new(&repo)).unwrap();
    std::fs::write(ready, "acquired").unwrap();
    if std::env::var("NEIGE_METADATA_TEST_EXEC").unwrap() == "1" {
        // Same PID stays alive after exec; the lock must not stay with it.
        panic!(
            "exec sleep failed: {}",
            Command::new("sleep").arg("20").exec()
        );
    }
    std::thread::sleep(Duration::from_secs(20));
}

#[test]
fn metadata_lock_process_exclusion_exit_release_and_cloexec() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo.git");
    bare_repo(&repo);
    let owner = GitMetadataLock::acquire(&repo).unwrap();
    let inode = owner._file.metadata().unwrap().ino();
    let ready = tmp.path().join("ready");
    let mut waiting = child(&repo, &ready, false);
    wait_ready(&mut waiting, &ready.with_extension("contended"));
    assert!(!ready.exists(), "other process acquired a held lock");
    assert!(waiting.0.try_wait().unwrap().is_none());
    drop(owner);
    wait_ready(&mut waiting, &ready);
    drop(waiting); // Abrupt process exit releases flock without a destructor.
    let owner = GitMetadataLock::acquire(&repo).unwrap();
    assert_eq!(
        owner._file.metadata().unwrap().ino(),
        inode,
        "stable persistent inode"
    );
    drop(owner);
    std::fs::remove_file(&ready).unwrap();
    let mut exec = child(&repo, &ready, true);
    wait_ready(&mut exec, &ready);
    let owner = GitMetadataLock::acquire(&repo).unwrap();
    assert!(
        exec.0.try_wait().unwrap().is_none(),
        "exec child still alive"
    );
    assert_eq!(owner._file.metadata().unwrap().ino(), inode);
}

#[test]
fn metadata_lock_refuses_symlink_and_nonregular_inodes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo.git");
    bare_repo(&repo);
    let lock = repo.join(LOCK_NAME.to_str().unwrap());
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, "untouched").unwrap();
    symlink(&victim, &lock).unwrap();
    assert!(
        GitMetadataLock::acquire(&repo).is_err(),
        "never follow a symlink to a regular file"
    );
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&victim, &lock).unwrap();
    assert!(
        GitMetadataLock::acquire(&repo).is_err(),
        "reject hardlink aliases"
    );
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    assert!(GitMetadataLock::acquire(&repo).is_err());
    std::fs::remove_dir(&lock).unwrap();
    let fifo = std::ffi::CString::new(lock.to_str().unwrap()).unwrap();
    // SAFETY: fifo is NUL terminated and lives for the syscall.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(
        GitMetadataLock::acquire(&repo).is_err(),
        "FIFO cannot block the file open"
    );
}
