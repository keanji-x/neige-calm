use calm_types::event::{ForgeEventSpec, ForgeMergeSubject};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::operation::forge_action_adapter::ProbeSpec;

#[derive(Debug, Deserialize)]
pub(crate) struct PluginForgePayload {
    pub(crate) argv: Vec<String>,
    pub(crate) idem_key: String,
    #[serde(default)]
    pub(crate) event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    pub(crate) subject: Option<ForgeMergeSubject>,
    #[serde(default)]
    pub(crate) context: serde_json::Map<String, Value>,
    #[serde(default)]
    pub(crate) probe: Option<ProbeSpec>,
    #[serde(default)]
    pub(crate) parked: bool,
}

/// Construct a forge action with event extraction and recovery probes.
pub(crate) fn forge_action_payload(
    idem_key: String,
    argv: Vec<String>,
    table: ForgeEventSpec,
    probes: ProbeSpec,
) -> PluginForgePayload {
    PluginForgePayload {
        argv,
        idem_key,
        event_spec: Some(table),
        subject: None,
        context: Map::new(),
        probe: Some(probes),
        parked: false,
    }
}
