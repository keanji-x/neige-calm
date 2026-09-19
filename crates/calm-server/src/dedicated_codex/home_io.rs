use std::fs::File;
use std::path::Path;

/// Atomically publish without replacing even an empty destination directory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_noreplace(from: &std::ffi::CStr, to: &std::ffi::CStr) -> std::io::Result<()> {
    // SAFETY: both borrowed C strings stay valid for the syscall. Neither
    // platform path has a check-then-rename or an overwrite fallback.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn rename_noreplace(
    _from: &std::ffi::CStr,
    _to: &std::ffi::CStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic home publication requires Linux or macOS",
    ))
}

pub(super) fn sync_directory(path: &Path) -> std::io::Result<()> {
    let directory = File::open(path)?;
    #[cfg(test)]
    faults::before_sync(path)?;
    directory.sync_all()
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, symlink},
    };

    fn publish(source: &Path, destination: &Path) -> std::io::Result<()> {
        rename_noreplace(
            &CString::new(source.as_os_str().as_bytes()).unwrap(),
            &CString::new(destination.as_os_str().as_bytes()).unwrap(),
        )
    }

    #[test]
    fn rename_noreplace_publishes_new_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("data"), "new").unwrap();
        publish(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("data")).unwrap(),
            "new"
        );
    }

    #[test]
    fn rename_noreplace_preserves_existing_empty_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("data"), "new").unwrap();
        std::fs::create_dir(&destination).unwrap();
        let inode = destination.metadata().unwrap().ino();
        assert_eq!(
            publish(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(destination.metadata().unwrap().ino(), inode);
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        assert_eq!(std::fs::read_to_string(source.join("data")).unwrap(), "new");
    }

    #[test]
    fn rename_noreplace_preserves_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, "new").unwrap();
        std::fs::write(&destination, "old").unwrap();
        assert_eq!(
            publish(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(source).unwrap(), "new");
    }

    #[test]
    fn rename_noreplace_preserves_existing_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, "new").unwrap();
        symlink("missing", &destination).unwrap();
        assert_eq!(
            publish(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read_link(destination).unwrap(),
            Path::new("missing")
        );
        assert_eq!(std::fs::read_to_string(source).unwrap(), "new");
    }
}

#[cfg(test)]
pub(super) mod faults {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct State {
        fail: Option<PathBuf>,
        before_publish: Option<Box<dyn FnOnce()>>,
    }
    thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

    pub struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            STATE.with(|state| *state.borrow_mut() = State::default());
        }
    }
    pub fn reset_on_drop() -> Reset {
        Reset
    }
    pub fn fail_sync(path: Option<PathBuf>) {
        STATE.with(|s| s.borrow_mut().fail = path);
    }
    pub fn before_sync(path: &Path) -> std::io::Result<()> {
        if STATE.with(|s| s.borrow().fail.as_deref() == Some(path)) {
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    }
    pub fn publish_once(callback: impl FnOnce() + 'static) {
        STATE.with(|s| s.borrow_mut().before_publish = Some(Box::new(callback)));
    }
    pub fn before_publish() {
        let callback = STATE.with(|s| s.borrow_mut().before_publish.take());
        if let Some(callback) = callback {
            callback();
        }
    }
}
