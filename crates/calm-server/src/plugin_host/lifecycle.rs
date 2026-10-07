use std::sync::Arc;
#[async_trait::async_trait]
pub trait LifecycleDb: Send + Sync {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, crate::error::CalmError>;
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), crate::error::CalmError>;
}
pub(super) struct Bridge(pub Arc<dyn LifecycleDb>);
#[async_trait::async_trait]
impl plugin::host::lifecycle::LifecycleDb<crate::error::CalmError> for Bridge {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, crate::error::CalmError> {
        self.0.enabled_row(id).await
    }
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), crate::error::CalmError> {
        self.0.set_enabled(id, enabled).await
    }
}
pub(crate) fn spawn_error_to_calm(error: super::HostError) -> crate::error::CalmError {
    plugin::host::lifecycle::spawn_error_to_calm(error)
}
#[cfg(test)]
mod spawn_error_mapping_tests {
    use super::spawn_error_to_calm;
    use crate::error::CalmError;
    use crate::plugin_host::HostError;
    use axum::http::StatusCode;

    #[test]
    fn template_conflict_maps_to_structured_409() {
        let mapped = spawn_error_to_calm(HostError::TemplateConflict {
            plugin_id: "dev.second".into(),
            template_id: "dev".into(),
            held_by: "dev.first".into(),
        });
        assert!(
            matches!(&mapped, CalmError::PluginConflict(msg)
                if msg.contains("dev") && msg.contains("dev.first")),
            "expected PluginConflict naming the template and holder, got {mapped:?}"
        );
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }

    #[test]
    fn kernel_too_old_still_maps_to_422() {
        let mapped =
            spawn_error_to_calm(HostError::KernelTooOld(crate::plugin_host::KernelTooOld {
                required: semver::Version::new(9, 9, 9),
                actual: semver::Version::new(0, 1, 0),
            }));
        assert_eq!(mapped.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(mapped.code(), "plugin_kernel_too_old");
    }

    #[test]
    fn operator_disabled_maps_to_structured_409() {
        let mapped = spawn_error_to_calm(HostError::OperatorDisabled("dev.app".into()));
        assert!(
            matches!(&mapped, CalmError::PluginConflict(msg)
                if msg.contains("dev.app") && msg.contains("enabled")),
            "expected PluginConflict naming the plugin and the bit, got {mapped:?}"
        );
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }

    /// `plugins_disabled` is a config file the running kernel cannot change, so `enable` is not its remedy.
    #[test]
    fn config_disabled_is_not_the_same_cell_as_operator_disabled() {
        let mapped = spawn_error_to_calm(HostError::Disabled("dev.app".into()));
        assert_eq!(mapped.code(), "internal");
        assert_eq!(mapped.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
