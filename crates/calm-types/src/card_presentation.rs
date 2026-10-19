//! Creation provenance and Terminal-owned inventory placement. Never an authorization fact.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

pub const TERMINAL_PRESENTATION_PAYLOAD_KEY: &str = "terminal_presentation";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum CardCreationSource {
    Manual,
    Agent,
    Task,
    System,
    Plugin,
    /// Original creation provenance is unavailable (history or an existing-card kind conversion).
    Unrecorded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TerminalPresentation {
    pub source: CardCreationSource,
    pub in_workbench: bool,
}

impl TerminalPresentation {
    pub fn created(source: CardCreationSource) -> Self {
        Self {
            source,
            in_workbench: matches!(source, CardCreationSource::Manual),
        }
    }

    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, String> {
        let value = payload
            .get(TERMINAL_PRESENTATION_PAYLOAD_KEY)
            .ok_or_else(|| "terminal presentation is missing".to_string())?;
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}
