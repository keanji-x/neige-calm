//! Cancellation-safe observation of one owner-retained task, without lifecycle policy.
pub(crate) enum RetainedJoin<T> {
    Running(tokio::task::JoinHandle<T>),
    Finished(std::result::Result<T, ()>),
}
impl<T> RetainedJoin<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self::Running(handle)
    }
    pub(crate) fn is_finished(&self) -> bool {
        match self {
            Self::Running(handle) => handle.is_finished(),
            Self::Finished(_) => true,
        }
    }
    pub(crate) async fn finish(&mut self) -> std::result::Result<&T, ()> {
        if let Self::Running(handle) = self {
            // Borrow rather than take: cancellation leaves the handle in its owner.
            let result = handle.await.map_err(|_| ());
            *self = Self::Finished(result);
        }
        match self {
            Self::Finished(Ok(result)) => Ok(result),
            Self::Finished(Err(())) | Self::Running(_) => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn acp_and_claude_retained_join_survives_cancelled_observer_and_caches_result() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let mut completion = RetainedJoin::new(tokio::spawn(async {
            wait.await.unwrap();
            7
        }));
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
        let mut completion = RetainedJoin::new(tokio::spawn(async { panic!("driver failed") }));
        assert!(completion.finish().await.is_err());
        assert!(completion.is_finished());
        assert!(completion.finish().await.is_err());
    }
}
