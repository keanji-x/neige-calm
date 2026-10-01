//! Named read-task policies are derived from the persisted task capability.
use super::*;
use crate::codex_appserver::PermissionsChoice;
use crate::operation::workspace_lease::task_guard::TaskWorkspaceGuard;

impl SharedCodexAppServer {
    pub(super) async fn read_task_permissions(
        &self,
        card: &str,
    ) -> Result<Option<PermissionsChoice>> {
        let Some(pool) = self.repo.sqlite_pool() else {
            return Ok(None);
        };
        let lease: Option<String> = sqlx::query_scalar(
            "SELECT lease_id FROM workspace_leases WHERE card_id=?1 AND holder_kind='task' \
             AND access_mode='read_only' ORDER BY created_at_ms DESC,lease_id DESC LIMIT 1",
        )
        .bind(card)
        .fetch_optional(&pool)
        .await?;
        let Some(lease) = lease else {
            return Ok(None);
        };
        let profile = TaskWorkspaceGuard::restore(&pool, &lease)
            .await?
            .into_read_profile(vec![
                self.protected_runtime_dir.clone(),
                self.home.path().to_owned(),
            ])
            .await?;
        self.home
            .ensure_task_read_profile(&profile)
            .map_err(|error| {
                CalmError::Conflict(format!("read permissions cannot be installed: {error}"))
            })?;
        Ok(Some(PermissionsChoice::NamedProfile(
            profile.name().to_owned(),
        )))
    }
}
