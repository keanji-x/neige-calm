//! Acquire the local execution resource before the caller revalidates its source.
use super::*;
use std::future::Future;

#[cfg(test)]
mod tests;

/// The caller owns admission; neither queueing nor a selected directory grants execution.
/// Owner/worker errors are outer; the inner result is the project's own evaluation.
pub(crate) async fn resolve_for_directory_admitted<F>(
    source: &Path,
    revalidate: impl FnOnce() -> F + Send,
) -> Result<std::result::Result<Environment, workspace::environment::EnvironmentError>>
where
    F: Future<Output = Result<()>> + Send,
{
    let source = source.to_owned();
    let base = directory_base_environment();
    let (ready, resource) = tokio::sync::oneshot::channel();
    let (release, admitted) = tokio::sync::oneshot::channel();
    let evaluation = tokio::task::spawn_blocking(move || {
        #[cfg(feature = "fixtures")]
        crate::test_seams::blocking_pause_point(
            "workspace-environment-before-ready",
            source.to_string_lossy().as_ref(),
        );
        if ready.send(()).is_err() || admitted.blocking_recv().is_err() {
            return None;
        }
        Some(workspace::environment::resolve(&source, base))
    });
    resource
        .await
        .map_err(|_| CalmError::Internal("workspace environment resource unavailable".into()))?;
    // Rejection or cancellation drops release. Even a detached blocking job then
    // exits without evaluating project code; a release is never cached or reused.
    revalidate().await?;
    release
        .send(())
        .map_err(|_| CalmError::Internal("workspace environment resource unavailable".into()))?;
    let environment = evaluation
        .await
        .map_err(|_| CalmError::Internal("workspace environment loader interrupted".into()))?
        .ok_or_else(|| CalmError::Internal("workspace environment execution cancelled".into()))?;
    Ok(environment.map(filter_environment))
}
