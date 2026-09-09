//! Fixture-only entry points at actual release and notice fences; no replacement logic.
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

// One-shot pause after real kernel observation, while Child remains unreaped.
static COMPLETION: LazyLock<Mutex<HashMap<String, Hook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct CandidateCompletionHook(String);
pub fn install_candidate_completion_hook(publication: &str, hook: Hook) -> CandidateCompletionHook {
    assert!(
        COMPLETION
            .lock()
            .unwrap()
            .insert(publication.into(), hook)
            .is_none()
    );
    CandidateCompletionHook(publication.into())
}
impl Drop for CandidateCompletionHook {
    fn drop(&mut self) {
        COMPLETION.lock().unwrap().remove(&self.0);
    }
}
pub(super) async fn before_completion(publication: &str, path: PathBuf) {
    let hook = COMPLETION.lock().unwrap().remove(publication);
    if let Some(hook) = hook {
        hook(path).await;
    }
}

// Pause after the read-only decision to exercise both stale-hint directions.
type HintHook = Arc<dyn Fn(bool) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;
static RECOVERY_HINT: LazyLock<Mutex<HashMap<String, HintHook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct CandidateRecoveryHintHook(String);
pub fn install_candidate_recovery_hint_hook(op: &str, hook: HintHook) -> CandidateRecoveryHintHook {
    assert!(
        RECOVERY_HINT
            .lock()
            .unwrap()
            .insert(op.into(), hook)
            .is_none()
    );
    CandidateRecoveryHintHook(op.into())
}
impl Drop for CandidateRecoveryHintHook {
    fn drop(&mut self) {
        RECOVERY_HINT.lock().unwrap().remove(&self.0);
    }
}
pub(super) async fn after_recovery_hint(op: &str, eligible: bool) {
    let hook = RECOVERY_HINT.lock().unwrap().remove(op);
    if let Some(hook) = hook {
        hook(eligible).await;
    }
}

/// Exercise the real notice relevance reader independently of Dispatcher prefix/dedup.
/// Fixture-only visibility; no replacement authorization or synthetic result.
pub async fn candidate_review_notice_relevant(
    repo: &dyn crate::db::RepoEventWrite,
    track: &crate::ids::TrackId,
    task: &str,
    operation: &str,
) -> crate::error::Result<bool> {
    crate::isolated_codex::settled::relevant(repo, track, task, operation).await
}
