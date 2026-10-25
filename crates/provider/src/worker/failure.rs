//! Provider-owned classification of native errors; no rollout reads or kernel policy.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureKind {
    ConcurrencyLimit,
    ContentRefusal,
    ProviderError,
}

impl WorkerFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConcurrencyLimit => "concurrency_limit",
            Self::ContentRefusal => "content_refusal",
            Self::ProviderError => "provider_error",
        }
    }
}

/// Bounded native evidence. `truncated` covers either the message or native code preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkerFailureCause {
    pub kind: WorkerFailureKind,
    pub native_code: Option<String>,
    pub message: String,
    pub truncated: bool,
}

impl WorkerFailureCause {
    pub(crate) fn from_codex(error: &crate::codex::TurnError) -> Self {
        let code = error.codex_error_info.as_ref().and_then(|info| {
            info.as_str().or_else(|| {
                let object = info.as_object()?;
                (object.len() == 1).then(|| object.keys().next().unwrap().as_str())
            })
        });
        let kind = match code {
            // App-server v2 uses camelCase, unlike rollout's cyber_policy.
            Some("cyberPolicy") => WorkerFailureKind::ContentRefusal,
            // Codex has no dedicated concurrency variant. Match only the known native
            // service message, in its owner; generic rate limits are not concurrency limits.
            _ if error
                .message
                .starts_with("Concurrency limit exceeded for user, please retry later") =>
            {
                WorkerFailureKind::ConcurrencyLimit
            }
            _ => WorkerFailureKind::ProviderError,
        };
        let message_end = error
            .message
            .floor_char_boundary(1024.min(error.message.len()));
        let native_code =
            code.map(|code| code[..code.floor_char_boundary(128.min(code.len()))].to_owned());
        Self {
            kind,
            native_code,
            message: error.message[..message_end].to_owned(),
            truncated: message_end < error.message.len()
                || code.is_some_and(|code| code.len() > 128),
        }
    }
}
