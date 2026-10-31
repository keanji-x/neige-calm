//! Cancellation-safe observation of one owner-retained task, without lifecycle policy.
use std::sync::{Arc, OnceLock};

pub(crate) struct RetainedJoin<T> {
    result: Arc<OnceLock<std::result::Result<T, ()>>>,
    ready: Arc<tokio::sync::Notify>,
    #[cfg(any(test, feature = "fixtures"))]
    abort: tokio::task::AbortHandle,
}
impl<T: Send + Sync + 'static> RetainedJoin<T> {
    pub(crate) fn new(
        handle: tokio::task::JoinHandle<T>,
        on_join: impl FnOnce() + Send + 'static,
    ) -> Self {
        let result = Arc::new(OnceLock::new());
        let ready = Arc::new(tokio::sync::Notify::new());
        let completion = Self {
            result: result.clone(),
            ready: ready.clone(),
            #[cfg(any(test, feature = "fixtures"))]
            abort: handle.abort_handle(),
        };
        tokio::spawn(async move {
            // The original task has actually exited before publishing readiness.
            // Errors remain errors: a hint never qualifies a successful END.
            let _ = result.set(handle.await.map_err(|_| ()));
            ready.notify_waiters();
            on_join();
        });
        completion
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.result.get().is_some()
    }
    pub(crate) async fn finish(&mut self) -> std::result::Result<&T, ()> {
        loop {
            let ready = self.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(result) = self.result.get() {
                return result.as_ref().map_err(|()| ());
            }
            ready.await;
        }
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn abort(&self) {
        self.abort.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn acp_and_claude_retained_join_survives_cancelled_observer_and_caches_result() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let mut completion = RetainedJoin::new(
            tokio::spawn(async {
                wait.await.unwrap();
                7
            }),
            || {},
        );
        assert!(!completion.is_finished());
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, completion.finish())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(*completion.finish().await.unwrap(), 7);
        assert!(completion.is_finished());
        assert_eq!(*completion.finish().await.unwrap(), 7);
    }
    #[tokio::test]
    async fn acp_and_claude_retained_join_caches_panic_without_fabricating_completion() {
        let mut completion =
            RetainedJoin::new(tokio::spawn(async { panic!("driver failed") }), || {});
        assert!(completion.finish().await.is_err());
        assert!(completion.is_finished());
        assert!(completion.finish().await.is_err());
    }
    #[tokio::test]
    async fn retained_join_wakes_without_an_observer_only_after_task_exit() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Exit(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Exit {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let exit = Exit(exited.clone());
        let task = tokio::spawn(async move {
            let _exit = exit;
            wait.await.unwrap();
            42
        });
        let (hint, hinted) = tokio::sync::oneshot::channel();
        let mut completion = RetainedJoin::new(task, move || {
            assert!(exited.load(std::sync::atomic::Ordering::SeqCst));
            hint.send(()).unwrap();
        });
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), hinted)
            .await
            .expect("task exit must wake the owner without a polling finish call")
            .unwrap();
        assert!(completion.is_finished());
        assert_eq!(*completion.finish().await.unwrap(), 42);
        assert_eq!(*completion.finish().await.unwrap(), 42);
    }
    #[tokio::test]
    async fn retained_join_abort_wakes_but_never_fabricates_success() {
        let (hint, hinted) = tokio::sync::oneshot::channel();
        let mut completion =
            RetainedJoin::new(tokio::spawn(std::future::pending::<bool>()), move || {
                hint.send(()).unwrap();
            });
        completion.abort();
        tokio::time::timeout(std::time::Duration::from_secs(1), hinted)
            .await
            .unwrap()
            .unwrap();
        assert!(completion.is_finished());
        assert!(completion.finish().await.is_err());
        assert!(completion.finish().await.is_err());
    }
}
