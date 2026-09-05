//! Current aliases and immutable execution/gate addresses share one log reader.
use std::path::Path;

use crate::model::{CardRole, Task, Track};

use super::{TrackFsContent, TrackFsError, TrackFsView, path_not_available};

/// Canonical virtual address for one gate event's full evidence. The existing
/// task ID delimiter ':' is preserved; gate attempts are a separate path segment
/// so neither URL fragments nor a current-task alias can change the subject.
pub fn task_gate_log_path(attempt_id: &str, gate_attempt: i64) -> Result<String, TrackFsError> {
    if attempt_id.is_empty()
        || matches!(attempt_id, "." | "..")
        || !attempt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        || gate_attempt < 1
    {
        return Err(path_not_available("invalid execution gate-log address"));
    }
    Ok(format!("runs/{attempt_id}/gates/{gate_attempt}.log"))
}

impl TrackFsView<'_> {
    fn gate_logs_directory(&self, path: &str) -> Result<&Path, TrackFsError> {
        let Some((role, directory)) = &self.gate_log_access else {
            return Err(TrackFsError::Forbidden(format!(
                "track_file: forbidden: {path} is not available on this surface"
            )));
        };
        if *role != CardRole::Planner {
            return Err(TrackFsError::Forbidden(format!(
                "track_file: forbidden: {path} is planner-only (§6.7); caller role {role:?}"
            )));
        }
        Ok(directory)
    }

    pub(super) async fn cat_execution_gate_log(
        &self,
        track: &Track,
        path: &str,
        attempt_id: &str,
        gate_file: &str,
    ) -> Result<TrackFsContent, TrackFsError> {
        // Enforce the same narrow role boundary before resolving a private run.
        self.gate_logs_directory(path)?;
        let gate_attempt = gate_file
            .strip_suffix(".log")
            .and_then(|number| number.parse::<i64>().ok())
            .ok_or_else(|| path_not_available(path))?;
        if task_gate_log_path(attempt_id, gate_attempt)? != path {
            return Err(path_not_available(path));
        }
        let task = self
            .repo
            .task_get(attempt_id)
            .await
            .map_err(|error| TrackFsError::Internal(format!("track_file: task lookup: {error}")))?
            .ok_or_else(|| path_not_available(path))?;
        if task.track_id != track.id.as_str() {
            return Err(TrackFsError::Forbidden(format!(
                "track_file: forbidden: execution {attempt_id} is not in the caller's bound track {}",
                track.id.as_str()
            )));
        }
        self.read_gate_log(&task, gate_attempt, path).await
    }

    /// Convenience alias follows the currently allocated execution and its most
    /// recent gate. Event/history observations must use task_gate_log_path.
    pub(super) async fn cat_gate_log(
        &self,
        track: &Track,
        key: &str,
    ) -> Result<TrackFsContent, TrackFsError> {
        let path = format!("plan/{key}/gate.log");
        self.gate_logs_directory(&path)?;
        let task = self
            .repo
            .task_current_get(track.id.as_str(), key)
            .await
            .map_err(|error| TrackFsError::Internal(format!("track_file: task lookup: {error}")))?
            .ok_or_else(|| path_not_available(&path))?;
        if task.gate_json.is_none() {
            return Err(path_not_available(&format!(
                "{path} (task declares no gate)"
            )));
        }
        if task.gate_attempt < 1 {
            return Err(path_not_available(&format!(
                "{path} (no gate attempt has run yet)"
            )));
        }
        self.read_gate_log(&task, task.gate_attempt, &path).await
    }

    async fn read_gate_log(
        &self,
        task: &Task,
        gate_attempt: i64,
        path: &str,
    ) -> Result<TrackFsContent, TrackFsError> {
        let directory = self.gate_logs_directory(path)?;
        // This guard also protects the on-disk join for legacy task IDs. Never
        // interpolate unchecked request or database bytes into filesystem paths.
        task_gate_log_path(&task.id, gate_attempt)?;
        if task.gate_json.is_none() {
            return Err(path_not_available(&format!(
                "{path} (task declares no gate)"
            )));
        }
        if gate_attempt > task.gate_attempt {
            return Err(path_not_available(&format!(
                "{path} (gate attempt has not run)"
            )));
        }
        let log_path = directory.join(format!("{}-g{gate_attempt}.log", task.id));
        match tokio::fs::read_to_string(&log_path).await {
            Ok(content) => Ok(TrackFsContent {
                content,
                content_type: "text/plain".into(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(path_not_available(
                &format!("{path} (log file not present yet)"),
            )),
            Err(error) => Err(TrackFsError::Internal(format!(
                "track_file: gate log read {}: {error}",
                log_path.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::task_gate_log_path;

    #[test]
    fn execution_gate_path_preserves_delimiters_and_rejects_unsafe_segments() {
        assert_eq!(
            task_gate_log_path("track-1:build.v2_a", 3).unwrap(),
            "runs/track-1:build.v2_a/gates/3.log"
        );
        for id in [
            "",
            ".",
            "..",
            "../other",
            "/absolute",
            "a/b",
            "a\\b",
            "a#g1",
            "a?gate=1",
            "a%2fb",
            "a\n",
            "a b",
        ] {
            assert!(task_gate_log_path(id, 1).is_err(), "{id:?}");
        }
        for gate_attempt in [i64::MIN, -1, 0] {
            assert!(task_gate_log_path("w:b", gate_attempt).is_err());
        }
    }
}
