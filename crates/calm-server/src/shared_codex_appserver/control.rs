//! Failure facts captured before native control errors enter the application error domain.
use crate::error::CalmError;

/// Only the shared daemon's control producers choose these operation facts. The payload is an
/// opaque diagnostic; consumers must never reclassify it by inspecting `CalmError`.
#[derive(Debug)]
pub enum ControlError {
    Refused(CalmError),
    InvalidRequest(CalmError),
    Unanswered(CalmError),
}

impl From<provider::codex::error::Error> for ControlError {
    fn from(error: provider::codex::error::Error) -> Self {
        use provider::codex::error::Error;
        match error {
            error @ Error::Refused(_) => Self::Refused(error.into()),
            error @ (Error::Transport(_) | Error::Serde(_)) => Self::Unanswered(error.into()),
        }
    }
}

impl From<ControlError> for CalmError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Refused(error)
            | ControlError::InvalidRequest(error)
            | ControlError::Unanswered(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_mapping_native_control_errors_are_classified_before_widening() {
        use provider::codex::error::Error;
        let decode = serde_json::from_str::<bool>("invalid").unwrap_err();
        let decode_text = decode.to_string();
        for (native, refused, expected) in [
            (
                Error::Refused("original refusal".into()),
                true,
                "codex refused: original refusal".to_owned(),
            ),
            (
                Error::Transport("original transport".into()),
                false,
                "codex app-server: original transport".to_owned(),
            ),
            (Error::Serde(decode), false, format!("serde: {decode_text}")),
        ] {
            let fact = ControlError::from(native);
            match &fact {
                ControlError::Refused(error) => {
                    assert!(refused);
                    assert!(matches!(error, CalmError::CodexRefused(_)));
                }
                ControlError::Unanswered(error) => {
                    assert!(!refused);
                    assert!(matches!(
                        error,
                        CalmError::CodexAppServer(_) | CalmError::Serde(_)
                    ));
                }
                ControlError::InvalidRequest(error) => {
                    panic!("native RPC did not produce an invalid request: {error}")
                }
            }
            let diagnostic = CalmError::from(fact);
            assert_eq!(diagnostic.to_string(), expected);
        }
    }
}
