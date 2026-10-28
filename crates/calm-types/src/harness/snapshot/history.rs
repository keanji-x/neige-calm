//! Read-only history protection across released checkpoint formats.
use super::CanonicalHarnessSnapshot;
use crate::harness::{HARNESS_MODE, LEGACY_HARNESS_SNAPSHOT_SCHEMA_VERSION};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct HarnessHistoryEndpoints {
    pub last_seen_head: Option<String>,
    pub issued_turn_head: Option<String>,
}

impl HarnessHistoryEndpoints {
    /// Validate canonical frames through their declared conversions. The released
    /// V1 queue codec lives in the server; its history projection deliberately
    /// reads only the two optional endpoints (absent and null both mean none).
    /// This never rewrites a checkpoint or grants permission to resume one.
    pub fn from_checkpoint(value: Value) -> Result<Self, String> {
        if value.get("schema_version").and_then(Value::as_u64)
            == Some(u64::from(LEGACY_HARNESS_SNAPSHOT_SCHEMA_VERSION))
        {
            if value.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE)
                || value.get("canonical_inputs").is_some()
            {
                return Err("invalid released V1 harness history projection".into());
            }
            return serde_json::from_value(value).map_err(|error| error.to_string());
        }
        let frame = CanonicalHarnessSnapshot::from_released(value)?;
        Ok(Self {
            last_seen_head: frame.header.last_seen_head,
            issued_turn_head: frame.header.issued_turn_head,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HARNESS_SNAPSHOT_SCHEMA_VERSION;
    use serde_json::json;

    #[test]
    fn current_codec_and_version_constant_agree() {
        let mut frame = CanonicalHarnessSnapshot::dormant(0);
        frame.header.last_seen_head = Some("seen".into());
        frame.header.issued_turn_head = Some("issued".into());
        let value = serde_json::to_value(frame).unwrap();
        assert_eq!(value["schema_version"], HARNESS_SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(
            HarnessHistoryEndpoints::from_checkpoint(value).unwrap(),
            HarnessHistoryEndpoints {
                last_seen_head: Some("seen".into()),
                issued_turn_head: Some("issued".into()),
            }
        );
    }

    #[test]
    fn released_v1_projection_preserves_optional_endpoints_and_rejects_ambiguity() {
        for heads in [
            json!({}),
            json!({"last_seen_head":null,"issued_turn_head":null}),
        ] {
            let mut value = json!({"schema_version":LEGACY_HARNESS_SNAPSHOT_SCHEMA_VERSION,"mode":HARNESS_MODE});
            value
                .as_object_mut()
                .unwrap()
                .extend(heads.as_object().unwrap().clone());
            assert_eq!(
                HarnessHistoryEndpoints::from_checkpoint(value).unwrap(),
                HarnessHistoryEndpoints {
                    last_seen_head: None,
                    issued_turn_head: None,
                }
            );
        }
        for (field, invalid) in [
            ("schema_version", json!(99)),
            ("mode", json!("other")),
            ("last_seen_head", json!(123)),
            ("issued_turn_head", json!([])),
            ("canonical_inputs", json!({})),
        ] {
            let mut value = json!({"schema_version":1,"mode":"harness"});
            value[field] = invalid;
            assert!(
                HarnessHistoryEndpoints::from_checkpoint(value).is_err(),
                "{field}"
            );
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({}),
            json!({"schema_version":"5"}),
        ] {
            assert!(HarnessHistoryEndpoints::from_checkpoint(invalid).is_err());
        }
    }
}
