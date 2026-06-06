//! Backend registry for kernel-owned card kinds.
//!
//! Plugin-defined card kinds remain opaque: if no handler matches a kind,
//! validation succeeds without inspecting the payload.

mod builtins;

use serde_json::Value;

use crate::actor::Actor;
use crate::error::CalmError;
use crate::ids::{CardId, WaveId};
use crate::state::AppState;

pub use builtins::{
    ClaudeCardHandler, CodexCardHandler, PluginUiCardHandler, SpecCardHandler, TerminalCardHandler,
    WaveReportCardHandler,
};

pub type CardKindResult<T> = std::result::Result<T, CardKindError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKindMatcher {
    Exact(&'static str),
    Prefix(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardCreateMode {
    Generic,
    Atomic,
    KernelMintedOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardPersistenceInvariants {
    pub deletable_after_create: bool,
    pub unique_per_wave: bool,
}

impl Default for CardPersistenceInvariants {
    fn default() -> Self {
        Self {
            deletable_after_create: true,
            unique_per_wave: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CardKindError {
    #[error("bad payload for {kind}: {message}")]
    BadPayload { kind: String, message: String },
    #[error("internal card kind error: {0}")]
    Internal(String),
}

impl From<CardKindError> for CalmError {
    fn from(value: CardKindError) -> Self {
        match value {
            CardKindError::BadPayload { kind, message } => {
                CalmError::BadRequest(format!("invalid {kind} payload: {message}"))
            }
            CardKindError::Internal(message) => CalmError::Internal(message),
        }
    }
}

#[async_trait::async_trait]
pub trait CardKindHandler: Send + Sync + 'static {
    fn kind_id(&self) -> &'static str;

    fn matcher(&self) -> CardKindMatcher {
        CardKindMatcher::Exact(self.kind_id())
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::Generic
    }

    fn schema_version(&self) -> Option<u32> {
        None
    }

    fn persistence_invariants(&self) -> CardPersistenceInvariants {
        CardPersistenceInvariants::default()
    }

    fn validate_payload(&self, payload: &Value) -> CardKindResult<()>;

    async fn on_delete(&self, _ctx: CardDeleteContext<'_>) -> CardKindResult<()> {
        Ok(())
    }
}

pub struct CardDeleteContext<'a> {
    pub app: &'a AppState,
    pub actor: &'a Actor,
    pub card_id: CardId,
    pub wave_id: WaveId,
    pub kind: &'a str,
}

pub struct CardKindRegistry {
    handlers: Vec<Box<dyn CardKindHandler>>,
}

impl CardKindRegistry {
    pub fn new(handlers: Vec<Box<dyn CardKindHandler>>) -> Self {
        Self { handlers }
    }

    pub fn builtins() -> Self {
        Self::new(vec![
            Box::new(TerminalCardHandler),
            Box::new(CodexCardHandler),
            Box::new(ClaudeCardHandler),
            Box::new(WaveReportCardHandler),
            Box::new(SpecCardHandler),
            Box::new(PluginUiCardHandler),
        ])
    }

    pub fn handler_for(&self, kind: &str) -> Option<&dyn CardKindHandler> {
        if let Some(handler) = self.handlers.iter().find(
            |handler| matches!(handler.matcher(), CardKindMatcher::Exact(exact) if exact == kind),
        ) {
            return Some(handler.as_ref());
        }

        let mut best: Option<(usize, &dyn CardKindHandler)> = None;
        for handler in &self.handlers {
            let CardKindMatcher::Prefix(prefix) = handler.matcher() else {
                continue;
            };
            if !kind.starts_with(prefix) {
                continue;
            }
            let prefix_len = prefix.len();
            if best.is_none_or(|(best_len, _)| prefix_len > best_len) {
                best = Some((prefix_len, handler.as_ref()));
            }
        }
        best.map(|(_, handler)| handler)
    }

    pub fn validate_payload(&self, kind: &str, payload: &Value) -> CardKindResult<()> {
        self.handler_for(kind)
            .map_or(Ok(()), |handler| handler.validate_payload(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    struct TestHandler {
        id: &'static str,
        matcher: CardKindMatcher,
    }

    impl CardKindHandler for TestHandler {
        fn kind_id(&self) -> &'static str {
            self.id
        }

        fn matcher(&self) -> CardKindMatcher {
            self.matcher
        }

        fn validate_payload(&self, _payload: &Value) -> CardKindResult<()> {
            Ok(())
        }
    }

    #[test]
    fn exact_match_wins_over_prefix() {
        let registry = CardKindRegistry::new(vec![
            Box::new(TestHandler {
                id: "prefix",
                matcher: CardKindMatcher::Prefix("term"),
            }),
            Box::new(TestHandler {
                id: "exact",
                matcher: CardKindMatcher::Exact("terminal"),
            }),
        ]);

        let handler = registry.handler_for("terminal").unwrap();
        assert_eq!(handler.kind_id(), "exact");
    }

    #[test]
    fn longest_prefix_wins_among_prefix_only() {
        let registry = CardKindRegistry::new(vec![
            Box::new(TestHandler {
                id: "short",
                matcher: CardKindMatcher::Prefix("ui://"),
            }),
            Box::new(TestHandler {
                id: "long",
                matcher: CardKindMatcher::Prefix("ui://chart/"),
            }),
        ]);

        let handler = registry.handler_for("ui://chart/foo").unwrap();
        assert_eq!(handler.kind_id(), "long");
    }

    #[test]
    fn same_priority_prefix_tie_keeps_first_registered() {
        let registry = CardKindRegistry::new(vec![
            Box::new(TestHandler {
                id: "first",
                matcher: CardKindMatcher::Prefix("ui://"),
            }),
            Box::new(TestHandler {
                id: "second",
                matcher: CardKindMatcher::Prefix("ui://"),
            }),
        ]);

        let handler = registry.handler_for("ui://plugin/foo").unwrap();
        assert_eq!(handler.kind_id(), "first");
    }

    #[test]
    fn unknown_kind_validates_ok() {
        let registry = CardKindRegistry::builtins();
        assert!(
            registry
                .validate_payload("future-thing", &Value::Null)
                .is_ok()
        );
    }

    #[test]
    fn ui_prefixed_kind_is_opaque_passthrough() {
        let registry = CardKindRegistry::builtins();
        assert!(
            registry
                .validate_payload("ui://plugin/foo", &json!({ "anything": 1 }))
                .is_ok()
        );
    }

    #[test]
    fn terminal_validates() {
        let registry = CardKindRegistry::builtins();
        registry.validate_payload("terminal", &Value::Null).unwrap();
        registry
            .validate_payload(
                "terminal",
                &json!({ "schemaVersion": 1, "terminal_id": "t1" }),
            )
            .unwrap();
        assert!(
            registry
                .validate_payload(
                    "terminal",
                    &json!({ "schemaVersion": 9999, "terminal_id": "t1" })
                )
                .is_err()
        );
        assert!(
            registry
                .validate_payload(
                    "terminal",
                    &json!({ "schemaVersion": 1, "terminal_id": 42 })
                )
                .is_err()
        );
    }

    #[test]
    fn wave_report_validates_required_shape() {
        let registry = CardKindRegistry::builtins();
        assert!(
            registry
                .validate_payload("wave-report", &json!({ "schemaVersion": 1, "body": "x" }))
                .is_err()
        );
        registry
            .validate_payload(
                "wave-report",
                &json!({ "schemaVersion": 1, "summary": "s", "body": "x" }),
            )
            .unwrap();
    }

    #[test]
    fn schema_version_constants_stay_wired() {
        let registry = CardKindRegistry::builtins();

        assert_eq!(
            registry.handler_for("terminal").unwrap().schema_version(),
            Some(crate::validation::TERMINAL_PAYLOAD_SCHEMA_VERSION)
        );
        assert_eq!(
            registry.handler_for("codex").unwrap().schema_version(),
            Some(crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION)
        );
        assert_eq!(
            registry.handler_for("claude").unwrap().schema_version(),
            Some(crate::validation::CLAUDE_PAYLOAD_SCHEMA_VERSION)
        );
        assert_eq!(
            registry
                .handler_for("wave-report")
                .unwrap()
                .schema_version(),
            Some(crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION)
        );
    }
}
