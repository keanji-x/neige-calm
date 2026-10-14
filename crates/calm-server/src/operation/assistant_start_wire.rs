//! New Assistant wire has required provider identity; historical operations remain byte-frozen.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::planner_harness_start_adapter::{HarnessProfile, PlannerHarnessStartOperationPayload};
use crate::error::{CalmError, Result};
use crate::session_projection_repo::AgentProvider;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum AssistantWireVersion {
    #[serde(rename = "assistant-v1")]
    V1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AssistantStartWire {
    pub version: AssistantWireVersion,
    pub provider: AgentProvider,
    #[serde(flatten)]
    pub payload: PlannerHarnessStartOperationPayload,
}

pub(crate) struct StartInput {
    pub payload: PlannerHarnessStartOperationPayload,
    pub mint_provider: AgentProvider,
}

impl StartInput {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        if value.get("version").is_some() || value.get("provider").is_some() {
            let wire: AssistantStartWire = serde_json::from_value(value.clone())?;
            if wire.payload.profile != HarnessProfile::Assistant {
                return Err(CalmError::BadRequest(
                    "Assistant wire requires Assistant profile".into(),
                ));
            }
            Ok(Self {
                payload: wire.payload,
                mint_provider: wire.provider,
            })
        } else {
            // Explicit historical compatibility: legacy Assistant and PlainChat mints were Codex.
            Ok(Self {
                payload: serde_json::from_value(value.clone())?,
                mint_provider: AgentProvider::Codex,
            })
        }
    }
}
