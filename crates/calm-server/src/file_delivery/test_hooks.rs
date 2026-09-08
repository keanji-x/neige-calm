//! Fixture-only pauses at the actual release fence; never a replacement launcher.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};
type Hook = Arc<dyn Fn(PathBuf) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;
static RELEASE: LazyLock<Mutex<HashMap<String, Hook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct CandidateReleaseHook(String);
pub fn install_candidate_release_hook(publication: &str, hook: Hook) -> CandidateReleaseHook {
    assert!(
        RELEASE
            .lock()
            .unwrap()
            .insert(publication.into(), hook)
            .is_none()
    );
    CandidateReleaseHook(publication.into())
}
impl Drop for CandidateReleaseHook {
    fn drop(&mut self) {
        RELEASE.lock().unwrap().remove(&self.0);
    }
}
pub(super) async fn before_release(publication: &str, path: PathBuf) {
    let hook = RELEASE.lock().unwrap().remove(publication);
    if let Some(hook) = hook {
        hook(path).await;
    }
}
