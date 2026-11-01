//! Feishu inspection owns polling, checkpoints, durable inboxes and quiet rounds.
pub mod model;
pub mod reader;
pub mod service;
#[cfg(test)]
mod tests;

pub const PLUGIN_ID: &str = "feishu-watch";
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
    use serde_json::json;
    let roles: &'static [CardRole] = &[CardRole::Planner];
    ["status","read","settle","reminders"].into_iter().map(|action| {
        let schema=match action {
            "status"=>json!({"type":"object","additionalProperties":false,"properties":{"after_batch_id":{"type":"string"},"after_source_id":{"type":"string"},"after_reminder_key":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":25}}}),
            "read"=>json!({"type":"object","additionalProperties":false,"required":["batch_id"],"properties":{"batch_id":{"type":"string"},"after_id":{"type":"string"},"legacy_offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":25}}}),
            "reminders"=>json!({"type":"object","additionalProperties":false,"required":["request_id","expected_revision","deadlines","cancel_deadline_keys"],"properties":{"request_id":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":"integer","minimum":1},"deadlines":{"type":"array","maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["key","at_ms","note"],"properties":{"key":{"type":"string","minLength":1,"maxLength":128},"at_ms":{"type":"integer"},"note":{"type":"string","minLength":1,"maxLength":2048}}}},"cancel_deadline_keys":{"type":"array","maxItems":100,"items":{"type":"string"}}}}),
            _=>json!({"type":"object","additionalProperties":false,"required":["batch_id","classification","deadlines","cancel_deadline_keys"],"properties":{"batch_id":{"type":"string"},"classification":{"type":"object"},"cancel_deadline_keys":{"type":"array","maxItems":100,"items":{"type":"string","minLength":1,"maxLength":128}},"deadlines":{"type":"array","maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["key","at_ms","note"],"properties":{"key":{"type":"string","minLength":1,"maxLength":128},"at_ms":{"type":"integer"},"note":{"type":"string","minLength":1,"maxLength":2048}}}}}}),
        };
        crate::builtin::tools::NativeToolSpec{name:action.into(),description:match action {
            "status"=>include_str!("prompts/tools/plugin_feishu_watch_status.md"),
            "read"=>include_str!("prompts/tools/plugin_feishu_watch_read.md"),
            "reminders"=>include_str!("prompts/tools/plugin_feishu_watch_reminders.md"),
            _=>include_str!("prompts/tools/plugin_feishu_watch_settle.md"),
        }.trim_end().into(),input_schema:schema,annotations:Some(if matches!(action,"settle"|"reminders") {
            calm_types::plugin::role_gated_write_annotations()
        } else {
            calm_types::plugin::read_only_annotations()
        }),roles,listed_for:roles}
    }).collect()
}

pub const FAILURE_NOTICE: &str = include_str!("prompts/inspection-failure.md");
pub const LEGACY_HELP: &str = include_str!("prompts/inspection-legacy-help.md");
