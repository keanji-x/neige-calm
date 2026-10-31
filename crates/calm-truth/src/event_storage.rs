//! Persisted field compaction, independent of public Event/wire serialization.
use crate::{
    compact_storage,
    error::{CalmError, Result},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const MARKER: &str = "_storage_fields";

fn fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "card.updated" => &["payload"],
        "track.report_edited" => &["body_before", "body_after"],
        _ => &[],
    }
}

pub(crate) fn encode_payload(kind: &str, mut payload: Value) -> Result<String> {
    let plain = serde_json::to_string(&payload)?;
    let selected = fields(kind);
    if selected.is_empty() || plain.len() > compact_storage::MAX_BLOCK_BYTES {
        return Ok(plain);
    }
    let Some(object) = payload.as_object_mut() else {
        return Ok(plain);
    };
    let mut bundle = Map::new();
    for field in selected {
        let value = object
            .remove(*field)
            .ok_or_else(|| CalmError::Internal("missing persisted event field".into()))?;
        bundle.insert((*field).into(), value);
    }
    let raw = serde_json::to_vec(&bundle)?;
    let Some(packed) = compact_storage::pack(&raw)? else {
        return Ok(plain);
    };
    object.insert(
        MARKER.into(),
        json!({"version":1,"data":STANDARD.encode(packed)}),
    );
    let encoded = serde_json::to_string(&payload)?;
    Ok(if encoded.len() < plain.len() {
        encoded
    } else {
        plain
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    data: String,
}

/// Legacy plain rows and compact rows produce identical logical JSON. The
/// discriminator is outside arbitrary Card.payload, which is never inspected.
pub(crate) fn hydrate(kind: &str, mut payload: Value) -> Result<Value> {
    let selected = fields(kind);
    if selected.is_empty() {
        return Ok(payload);
    }
    let Some(object) = payload.as_object_mut() else {
        return Ok(payload);
    };
    let Some(marker) = object.remove(MARKER) else {
        return Ok(payload);
    };
    let envelope: Envelope = serde_json::from_value(marker)?;
    if envelope.version != 1
        || envelope.data.len() > compact_storage::MAX_BLOCK_BYTES.div_ceil(3) * 4
    {
        return Err(CalmError::Internal(
            "unsupported or oversized event storage envelope".into(),
        ));
    }
    let bytes = STANDARD
        .decode(envelope.data)
        .map_err(|_| CalmError::Internal("invalid event storage encoding".into()))?;
    if bytes.len() > compact_storage::MAX_BLOCK_BYTES || !bytes.starts_with(b"NCZ") {
        return Err(CalmError::Internal("missing compact event frame".into()));
    }
    let decoded = compact_storage::unpack(&bytes)?;
    let bundle: Map<String, Value> = serde_json::from_slice(&decoded)?;
    if bundle.len() != selected.len()
        || selected
            .iter()
            .any(|k| !bundle.contains_key(*k) || object.contains_key(*k))
        || (kind == "track.report_edited" && bundle.values().any(|value| !value.is_string()))
    {
        return Err(CalmError::Internal(
            "ambiguous persisted event fields".into(),
        ));
    }
    object.extend(bundle);
    Ok(payload)
}

pub fn decode_payload(kind: &str, text: &str) -> Result<Value> {
    hydrate(kind, serde_json::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_fields_roundtrip_without_interpreting_user_payload_markers() {
        for (kind, value) in [
            (
                "card.updated",
                json!({"id":"card","track_id":"track","payload":{"_storage_fields":{"version":999},"text":"stored report text ".repeat(2048)}}),
            ),
            (
                "track.report_edited",
                json!({"card_id":"card","track_id":"track","author":"user","body_before":"old report text ".repeat(2048),"body_after":"new report text ".repeat(2048)}),
            ),
        ] {
            let plain = serde_json::to_string(&value).unwrap();
            let encoded = encode_payload(kind, value.clone()).unwrap();
            assert!(encoded.len() < plain.len() / 2);
            let stored: Value = serde_json::from_str(&encoded).unwrap();
            assert_eq!(stored["track_id"], value["track_id"]);
            assert_eq!(decode_payload(kind, &encoded).unwrap(), value);
            assert_eq!(decode_payload(kind, &plain).unwrap(), value);
        }
        let opaque = json!({"_storage_fields":{"version":999}});
        assert_eq!(hydrate("plugin.state", opaque.clone()).unwrap(), opaque);
    }

    #[test]
    fn malformed_compact_fields_fail_instead_of_hiding_or_overwriting_data() {
        let encoded = encode_payload(
            "card.updated",
            json!({"id":"card","payload":{"text":"report text ".repeat(2048)}}),
        )
        .unwrap();
        let stored: Value = serde_json::from_str(&encoded).unwrap();
        let mut future = stored.clone();
        future[MARKER]["version"] = json!(2);
        let mut duplicate = stored.clone();
        duplicate["payload"] = json!({"injected":true});
        let mut bad_base64 = stored.clone();
        bad_base64[MARKER]["data"] = json!("invalid!");
        let mut extra = stored.clone();
        extra[MARKER]["unknown"] = json!(true);
        for bad in [future, duplicate, bad_base64, extra] {
            assert!(hydrate("card.updated", bad).is_err());
        }
        for bundle in [
            json!({"body_before":123,"body_after":"large report ".repeat(1024)}),
            json!({"body_after":"large report ".repeat(1024)}),
        ] {
            let packed = compact_storage::pack(&serde_json::to_vec(&bundle).unwrap())
                .unwrap()
                .unwrap();
            assert!(
                hydrate(
                    "track.report_edited",
                    json!({MARKER:{"version":1,"data":STANDARD.encode(packed)}}),
                )
                .is_err()
            );
        }
        let valid = encode_payload(
            "card.updated",
            json!({"id":"card","payload":{"text":"not repetitive values"}}),
        )
        .unwrap();
        assert!(!valid.contains(MARKER));
    }
}
