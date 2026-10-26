//! Persisted retained activation wire codec. Lifecycle policy stays with its callers.
use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedPayload {
    pub canonical_activation_version: u32,
    pub card_id: String,
    pub worker_session_id: String,
}

impl RetainedPayload {
    pub fn card_id(&self) -> &str {
        &self.card_id
    }

    pub fn decode(value: &Value) -> Result<Option<Self>, CoreError> {
        if value.get("canonical_activation_version").is_none() {
            return Ok(None);
        }
        let payload: Self = serde_json::from_value(value.clone())?;
        if payload.canonical_activation_version != 2
            || payload.card_id.is_empty()
            || payload.worker_session_id.is_empty()
        {
            return Err(CoreError::Conflict(
                "unsupported retained activation".into(),
            ));
        }
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_wire_strict_contract() {
        let value =
            json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":"worker"});
        let wire = RetainedPayload::decode(&value).unwrap().unwrap();
        assert_eq!(serde_json::to_value(wire).unwrap(), value);
        assert!(
            RetainedPayload::decode(&json!({"planner_card_id":"old"}))
                .unwrap()
                .is_none()
        );
        for bad in [
            json!({"canonical_activation_version":3,"card_id":"card","worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":"","worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":""}),
        ] {
            assert!(matches!(
                RetainedPayload::decode(&bad),
                Err(CoreError::Conflict(_))
            ));
        }
        for bad in [
            json!({"canonical_activation_version":null,"card_id":"card","worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":4,"worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":"card"}),
            json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":"worker","extra":true}),
        ] {
            assert!(matches!(
                RetainedPayload::decode(&bad),
                Err(CoreError::Serde(_))
            ));
        }
    }
}
