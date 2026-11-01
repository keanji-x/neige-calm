//! Feishu inspection owns polling, checkpoints, durable inboxes and quiet rounds.
pub mod model;
pub mod reader;
pub mod service;
#[cfg(test)]
mod tests;

pub const PLUGIN_ID: &str = "feishuwatch";
pub const MAX_STATE_BYTES: usize = 1024 * 1024;

#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    async fn load(&self) -> Result<Option<model::State>, String>;
    /// Atomically fence lifecycle/config/Track/revision, store state and append a kernel wake.
    async fn commit(
        &self,
        expected_revision: Option<u64>,
        state: model::State,
        wake: Option<model::Wake>,
    ) -> Result<(), String>;
}

pub const MANIFEST: &str = include_str!("manifest.json");
pub const INSTRUCTIONS: &str = include_str!("instructions.md");
pub fn descriptors() -> Vec<crate::builtin::tools::NativeToolSpec> {
    use calm_types::model::CardRole;
    let schemas: serde_json::Value = serde_json::from_str(include_str!("schemas.json"))
        .expect("compiled inspection tool schemas");
    let roles: &'static [CardRole] = &[CardRole::Planner];
    let descriptions = [
        (
            "status",
            include_str!("prompts/tools/plugin_feishuwatch_status.md"),
        ),
        (
            "batch_cat",
            include_str!("prompts/tools/plugin_feishuwatch_batch_cat.md"),
        ),
        (
            "settle",
            include_str!("prompts/tools/plugin_feishuwatch_settle.md"),
        ),
        (
            "reminder_set",
            include_str!("prompts/tools/plugin_feishuwatch_reminder_set.md"),
        ),
        (
            "reminder_rm",
            include_str!("prompts/tools/plugin_feishuwatch_reminder_rm.md"),
        ),
    ];
    descriptions
        .into_iter()
        .map(
            |(name, description)| crate::builtin::tools::NativeToolSpec {
                name: name.into(),
                description: description.trim_end().into(),
                input_schema: schemas[name].clone(),
                annotations: Some(
                    if matches!(name, "settle" | "reminder_set" | "reminder_rm") {
                        calm_types::plugin::role_gated_write_annotations()
                    } else {
                        calm_types::plugin::read_only_annotations()
                    },
                ),
                roles,
                listed_for: roles,
            },
        )
        .collect()
}

pub const FAILURE_NOTICE: &str = include_str!("prompts/inspection-failure.md");
pub const LEGACY_HELP: &str = include_str!("prompts/inspection-legacy-help.md");
