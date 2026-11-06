//! Shared selection arguments for terminal and stream-json Claude launches.
use crate::TurnModelSelection;

pub fn selection_args(selection: &TurnModelSelection) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = &selection.model {
        args.push(format!("--model={model}"));
    }
    if let Some(effort) = &selection.effort {
        args.push(format!("--effort={effort}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_model_defaults_claude_arguments_are_single_tokens_without_fallback() {
        assert!(selection_args(&TurnModelSelection::inherit()).is_empty());
        assert_eq!(
            selection_args(&TurnModelSelection {
                model: Some("--private'$(false)".into()),
                effort: Some("future".into())
            }),
            ["--model=--private'$(false)", "--effort=future"]
        );
    }
}
