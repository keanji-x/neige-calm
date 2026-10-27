use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::os::unix::fs::MetadataExt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;

use super::super::{APPEND_AFTER_OPEN, append_log_line};

#[test]
fn append_waits_for_visible_line() {
    // Keep all cleanup owners outside catch_unwind. Runtime drop also drains a mandatory
    // file write if an assertion panics while the pinned helper is still pending.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("gate.log");
    std::fs::write(&path, "existing step\n").unwrap();
    let original = std::fs::metadata(&path).unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = Some(release_tx);
    let holder = Arc::new(Mutex::new(None));
    let hook_holder = Arc::clone(&holder);
    let result = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async {
            let (started_tx, started_rx) = mpsc::channel();
            let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let hook_reached = Arc::clone(&reached);
            let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
                let job = tokio::task::spawn_blocking(move || {
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                });
                *hook_holder.lock().unwrap() = Some(job);
                started_rx.recv().unwrap();
                hook_reached.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let append = APPEND_AFTER_OPEN.scope(
                RefCell::new(Some(hook)),
                append_log_line(&path, "gate RESULT DISCARDED: controlled append"),
            );
            tokio::pin!(append);
            // Follow open's actual wakeups. Once the hook confirms the only blocking
            // worker is occupied, this very poll reaches write_all (and, with the fix,
            // flush). No fixed poll count or scheduler timing assumption is needed.
            let returned = poll_fn(|cx| {
                let state = append.as_mut().poll(cx);
                if reached.load(std::sync::atomic::Ordering::SeqCst) {
                    Poll::Ready(state.is_ready())
                } else {
                    assert!(state.is_pending(), "append bypassed the after-open hook");
                    Poll::Pending
                }
            })
            .await;
            let held = std::fs::metadata(&path).unwrap();
            assert_eq!((held.dev(), held.ino()), (original.dev(), original.ino()));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing step\n");
            assert!(
                !returned,
                "append returned before its queued write was visible"
            );
            release.take().unwrap().send(()).unwrap();
            let job = holder.lock().unwrap().take().unwrap();
            job.await.unwrap();
            // Joining the holder alone does not prove the queued write completed.
            append.await;
            let current = std::fs::metadata(&path).unwrap();
            assert_eq!(
                (current.dev(), current.ino()),
                (original.dev(), original.ino())
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "existing step\ngate RESULT DISCARDED: controlled append\n"
            );
        });
    }));
    // Release and join even after panic, before dropping the runtime or temp directory.
    if let Some(release) = release.take() {
        let _ = release.send(());
    }
    let job = holder.lock().unwrap().take();
    if let Some(job) = job {
        runtime.block_on(job).unwrap();
    }
    drop(runtime);
    if let Err(panic) = result {
        resume_unwind(panic);
    }
}
