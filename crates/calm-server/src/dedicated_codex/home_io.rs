use std::fs::File;
use std::path::Path;

pub(super) fn sync_directory(path: &Path) -> std::io::Result<()> {
    let directory = File::open(path)?;
    #[cfg(test)]
    faults::before_sync(path)?;
    directory.sync_all()
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
