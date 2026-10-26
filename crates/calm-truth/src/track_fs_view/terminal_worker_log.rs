//! A bound track can read a terminal worker log at its kernel-persisted preparation address.
use super::{TrackFsContent, TrackFsError, TrackFsView, path_not_available};
use crate::model::Track;

impl TrackFsView<'_> {
    pub(super) async fn cat_terminal_worker_log(
        &self,
        track: &Track,
        path: &str,
        attempt: &str,
    ) -> Result<TrackFsContent, TrackFsError> {
        // Match the gate-log surface restriction: only callers with a bound identity enable logs.
        if self.gate_log_access.is_none() {
            return Err(TrackFsError::Forbidden(format!(
                "{path} is not available on this surface"
            )));
        }
        let task = self
            .repo
            .task_get(attempt)
            .await
            .map_err(|e| TrackFsError::Internal(e.to_string()))?
            .ok_or_else(|| path_not_available(path))?;
        if task.track_id != track.id.as_str() {
            return Err(TrackFsError::Forbidden(format!(
                "execution {attempt} is not in the caller's bound track"
            )));
        }
        let log = self
            .repo
            .terminal_worker_log_path(attempt)
            .await
            .map_err(|e| TrackFsError::Internal(e.to_string()))?;
        let log = log.ok_or_else(|| path_not_available(path))?;
        let content = tokio::fs::read_to_string(&log).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                path_not_available(path)
            } else {
                TrackFsError::Internal(format!("terminal log read: {e}"))
            }
        })?;
        Ok(TrackFsContent {
            content,
            content_type: "text/plain".into(),
        })
    }
}
