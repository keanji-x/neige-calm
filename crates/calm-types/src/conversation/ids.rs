//! Canonical input identities. Minting belongs to the persistence owner.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind} must be exactly {length} lowercase hexadecimal characters")]
pub struct ConversationIdError {
    kind: &'static str,
    length: usize,
}

macro_rules! input_id {
    ($name:ident, $length:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, ConversationIdError> {
                Self::try_from(value.to_owned())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ConversationIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.len() != $length
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(ConversationIdError {
                        kind: stringify!($name),
                        length: $length,
                    });
                }
                Ok(Self(value))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

input_id!(ConversationMessageId, 32);
input_id!(SystemObservationId, 32);
input_id!(LogicalTurnId, 32);
input_id!(InputSubmissionId, 32);
input_id!(InputAttemptId, 32);
input_id!(RetryIntentId, 32);
input_id!(PrivateHistoryOperationId, 32);
input_id!(SubmissionRenderDigest, 64);

/// The exact dispatch attempt whose crossing was checkpointed before calling a provider.
/// This is correlation, not evidence that a prompt was delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAttempt {
    pub submission_id: InputSubmissionId,
    pub attempt_id: InputAttemptId,
}
