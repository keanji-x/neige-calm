//! Registered internal Assistant producers and immutable original acceptance facts.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum SystemAssistantSource {
    TodaySummary,
    WorkerWatcher,
}
impl SystemAssistantSource {
    pub fn bootstrap_subject(self) -> &'static str {
        match self {
            Self::TodaySummary | Self::WorkerWatcher => "bootstrap",
        }
    }
    pub fn namespace(self) -> &'static str {
        match self {
            Self::TodaySummary => "today-summary",
            Self::WorkerWatcher => "worker-watcher",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemAssistantReceiptVersion {
    SystemAssistantV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SystemAssistantReceiptKind {
    Bootstrap {
        creation_floor: i64,
    },
    Input,
    WatcherBootstrap {
        creation_floor: i64,
        observation: QuietObservationFact,
    },
    WatcherInput {
        observation: QuietObservationFact,
    },
}
/// Immutable historical data. This is deliberately not a live renderer capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietObservationFact {
    pub version: u32,
    pub database_id: String,
    pub track_id: String,
    pub area_id: String,
    pub task_key: String,
    pub attempt_id: String,
    pub worker_card_id: String,
    pub worker_session_id: String,
    pub terminal_id: String,
    pub renderer_id: String,
    pub last_output_ms: i64,
    pub observed_at_ms: i64,
    pub quiet_after_ms: i64,
}
impl QuietObservationFact {
    pub fn subject(&self) -> String {
        format!("{}:{}", self.attempt_id, self.last_output_ms)
    }
}
impl SystemAssistantReceiptKind {
    pub fn is_bootstrap(&self) -> bool {
        matches!(self, Self::Bootstrap { .. } | Self::WatcherBootstrap { .. })
    }
    pub fn quiet_observation(&self) -> Option<&QuietObservationFact> {
        match self {
            Self::WatcherBootstrap { observation, .. } | Self::WatcherInput { observation } => {
                Some(observation)
            }
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemAssistantReceipt {
    pub version: SystemAssistantReceiptVersion,
    pub source: SystemAssistantSource,
    pub subject: String,
    pub card_id: String,
    pub track_id: String,
    pub original_worker: String,
    pub event_id: i64,
    pub event_fingerprint: String,
    pub observation_id: String,
    pub queued_at_ms: i64,
    pub presentation_sequence: i64,
    pub payload_digest: String,
    pub acceptance: SystemAssistantReceiptKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_source_roundtrips_and_refuses_unregistered_namespaces() {
        let source = SystemAssistantSource::TodaySummary;
        assert_eq!(serde_json::to_value(source).unwrap(), source.namespace());
        assert_eq!(
            serde_json::from_value::<SystemAssistantSource>(serde_json::json!(source.namespace()))
                .unwrap(),
            source
        );
        assert!(
            serde_json::from_value::<SystemAssistantSource>(serde_json::json!("unregistered"))
                .is_err()
        );
        assert_eq!(source.bootstrap_subject(), "bootstrap");
    }
}
