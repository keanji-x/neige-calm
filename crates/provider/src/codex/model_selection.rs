//! Codex configuration spelling shared by thread mint, cold resume and CLI attachment.
use crate::TurnModelSelection;
use serde_json::{Map, Value};

pub fn apply_thread_model_selection(
    config: &mut Map<String, Value>,
    selection: &TurnModelSelection,
) {
    if let Some(model) = &selection.model {
        config.insert("model".into(), Value::String(model.clone()));
    }
    if let Some(effort) = &selection.effort {
        config.insert(
            "model_reasoning_effort".into(),
            Value::String(effort.clone()),
        );
    }
}

/// `codex resume` accepts the same config overrides as a new interactive session.
/// Keep the TUI's later turns on the persisted selection as well as the native first turn.
pub fn selection_args(selection: &TurnModelSelection) -> Vec<String> {
    let mut config = Map::new();
    apply_thread_model_selection(&mut config, selection);
    config
        .into_iter()
        .map(|(key, value)| {
            let value = toml_edit::Value::from(value.as_str().expect("selection is a string"));
            format!("--config={key}={value}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_model_defaults_codex_config_preserves_pair_and_other_config() {
        let mut config = serde_json::json!({"shell_environment_policy":{"set":{"X":"Y"}}})
            .as_object()
            .unwrap()
            .clone();
        let old = config.clone();
        apply_thread_model_selection(&mut config, &TurnModelSelection::inherit());
        assert_eq!(config, old);
        apply_thread_model_selection(
            &mut config,
            &TurnModelSelection {
                model: Some("private".into()),
                effort: Some("future".into()),
            },
        );
        assert_eq!(config["model"], "private");
        assert_eq!(config["model_reasoning_effort"], "future");
        assert_eq!(
            config["shell_environment_policy"],
            old["shell_environment_policy"]
        );
        assert!(selection_args(&TurnModelSelection::inherit()).is_empty());
        let pair = TurnModelSelection {
            model: Some("private'$(false)\"\\".into()),
            effort: Some("future".into()),
        };
        let args = selection_args(&pair);
        let doc = args
            .iter()
            .map(|arg| arg.strip_prefix("--config=").unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(doc["model"].as_str(), pair.model.as_deref());
        assert_eq!(
            doc["model_reasoning_effort"].as_str(),
            pair.effort.as_deref()
        );
    }
}
