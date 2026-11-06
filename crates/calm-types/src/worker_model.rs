//! Publication-owned worker choices. Absence in an old card is not inheritance.
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

pub const WORKER_MODEL_SELECTION_KEY: &str = "worker_model_selection";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerModelProvider {
    Codex,
    Claude,
}
impl WorkerModelProvider {
    pub const fn setting_key(self) -> &'static str {
        match self {
            Self::Codex => "worker_model_defaults_codex",
            Self::Claude => "worker_model_defaults_claude",
        }
    }
}

/// Both members must be present, including when the provider should inherit one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerModelDefaults {
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required = true, nullable = true)]
    pub model: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required = true, nullable = true)]
    pub effort: Option<String>,
}
fn required_nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
impl WorkerModelDefaults {
    pub fn parse(value: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(value).map_err(|e| e.to_string())?;
        parsed.validate()?;
        Ok(parsed)
    }
    pub fn validate(&self) -> Result<(), String> {
        for (key, value) in [("model", &self.model), ("effort", &self.effort)] {
            if let Some(value) = value
                && (value.is_empty()
                    || value.trim() != value
                    || value.chars().any(char::is_control))
            {
                return Err(format!(
                    "{key} must be null or a nonempty string without surrounding whitespace or control characters"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerModelChoice {
    Inherit,
    Explicit(WorkerModelDefaults),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerModelSelection {
    pub provider: WorkerModelProvider,
    pub selection: WorkerModelChoice,
}
impl WorkerModelSelection {
    /// None means pre-upgrade. A present malformed value is always an error.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Option<Self>, String> {
        let Some(value) = payload.get(WORKER_MODEL_SELECTION_KEY) else {
            return Ok(None);
        };
        let snapshot: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if let WorkerModelChoice::Explicit(pair) = &snapshot.selection {
            pair.validate()?;
        }
        Ok(Some(snapshot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn worker_model_defaults_pair_requires_both_members_and_valid_strings() {
        for input in [
            "{}",
            r#"{"model":null}"#,
            r#"{"effort":null}"#,
            r#"{"model":"x","effort":42}"#,
            r#"{"model":" x","effort":null}"#,
            r#"{"model":"x","effort":""}"#,
            r#"{"model":"x","effort":null,"extra":true}"#,
            r#"{"model":"x","model":"y","effort":null}"#,
        ] {
            assert!(WorkerModelDefaults::parse(input).is_err(), "{input}");
        }
        assert_eq!(
            WorkerModelDefaults::parse(r#"{"model":null,"effort":null}"#).unwrap(),
            WorkerModelDefaults {
                model: None,
                effort: None
            }
        );
        assert!(
            WorkerModelDefaults::parse(r#"{"model":"private'$(false)","effort":"future"}"#).is_ok()
        );
    }
    #[test]
    fn worker_model_defaults_legacy_inherit_and_corrupt_are_distinct() {
        assert_eq!(
            WorkerModelSelection::from_payload(&json!({})).unwrap(),
            None
        );
        let snapshot = WorkerModelSelection {
            provider: WorkerModelProvider::Codex,
            selection: WorkerModelChoice::Inherit,
        };
        assert_eq!(
            WorkerModelSelection::from_payload(&json!({WORKER_MODEL_SELECTION_KEY:snapshot}))
                .unwrap(),
            Some(snapshot)
        );
        for value in [
            json!(null),
            json!(42),
            json!({"provider":"codex"}),
            json!({"provider":"unknown","selection":{"kind":"inherit"}}),
        ] {
            assert!(
                WorkerModelSelection::from_payload(&json!({WORKER_MODEL_SELECTION_KEY:value}))
                    .is_err()
            );
        }
    }
}
