//! Per-thread syscall-boundary injection; production builds contain no hook.
use crate::Result;
use std::{cell::RefCell, path::Path};

type SyncHook = Box<dyn FnMut(&Path) -> Result<()>>;
thread_local! {
    static SYNC_HOOK: RefCell<Option<SyncHook>> = const { RefCell::new(None) };
}

pub(crate) fn before_sync(path: &Path) -> Result<()> {
    SYNC_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(path),
        None => Ok(()),
    })
}

pub(crate) fn with_sync<T>(
    hook: impl FnMut(&Path) -> Result<()> + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SYNC_HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    SYNC_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "nested filesystem fault hook");
        *slot = Some(Box::new(hook));
    });
    let _reset = Reset;
    operation()
}
