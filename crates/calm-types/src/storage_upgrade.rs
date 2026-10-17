//! The verified maintenance child's result data, not a maintenance grant or
//! proof that a repository is ready. The updater and kernel validate their real
//! process/database association through their own held owners.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The writer format actually observed after the atomic conversation import.
/// A structural migration alone cannot produce this observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStorageEpoch {
    CanonicalInputsV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreparationResultVersion {
    V1,
}

/// Required association data on the existing direct-child result
/// channel. Successfully decoding it does not establish authority or readiness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PreparationResultWire")]
pub struct StoragePreparationResult {
    version: PreparationResultVersion,
    transition_id: String,
    database_id: String,
    writer_epoch: ConversationStorageEpoch,
    target_binary: PathBuf,
    target_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationResultWire {
    version: PreparationResultVersion,
    transition_id: String,
    database_id: String,
    writer_epoch: ConversationStorageEpoch,
    target_binary: PathBuf,
    target_sha256: String,
}

impl StoragePreparationResult {
    pub fn new(
        transition_id: String,
        database_id: String,
        writer_epoch: ConversationStorageEpoch,
        target_binary: PathBuf,
        target_sha256: String,
    ) -> Result<Self, &'static str> {
        if transition_id.is_empty() || database_id.is_empty() {
            return Err("prepared storage requires transition and database identities");
        }
        if !target_binary.is_absolute() {
            return Err("prepared storage requires an absolute target binary path");
        }
        if target_sha256.len() != 64
            || !target_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("prepared target hash must be 64 lowercase hexadecimal characters");
        }
        Ok(Self {
            version: PreparationResultVersion::V1,
            transition_id,
            database_id,
            writer_epoch,
            target_binary,
            target_sha256,
        })
    }

    pub fn transition_id(&self) -> &str {
        &self.transition_id
    }
    pub fn database_id(&self) -> &str {
        &self.database_id
    }
    pub fn writer_epoch(&self) -> ConversationStorageEpoch {
        self.writer_epoch
    }
    pub fn target_binary(&self) -> &Path {
        &self.target_binary
    }
    pub fn target_sha256(&self) -> &str {
        &self.target_sha256
    }
}

impl TryFrom<PreparationResultWire> for StoragePreparationResult {
    type Error = &'static str;

    fn try_from(value: PreparationResultWire) -> Result<Self, Self::Error> {
        let PreparationResultVersion::V1 = value.version;
        Self::new(
            value.transition_id,
            value.database_id,
            value.writer_epoch,
            value.target_binary,
            value.target_sha256,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepared() -> StoragePreparationResult {
        StoragePreparationResult::new(
            "held-transition".into(),
            "observed-database-identity".into(),
            ConversationStorageEpoch::CanonicalInputsV2,
            std::env::current_dir().unwrap().join("verified-target"),
            "a".repeat(64),
        )
        .unwrap()
    }

    #[test]
    fn prepared_result_round_trips_required_association_data() {
        let value = prepared();
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["version"], "v1");
        assert_eq!(json["writer_epoch"], "canonical_inputs_v2");
        let decoded: StoragePreparationResult = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.transition_id(), "held-transition");
        assert_eq!(decoded.database_id(), "observed-database-identity");
        assert_eq!(decoded.target_binary(), value.target_binary());
        assert_eq!(decoded.target_sha256(), "a".repeat(64));
    }

    #[test]
    fn prepared_result_never_defaults_an_epoch_or_accepts_unknown_fields_versions() {
        let json = serde_json::to_value(prepared()).unwrap();
        for field in [
            "transition_id",
            "database_id",
            "writer_epoch",
            "target_binary",
            "target_sha256",
        ] {
            let mut missing = json.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<StoragePreparationResult>(missing).is_err(),
                "{field}"
            );
        }
        let mut wrong = json.clone();
        wrong["version"] = json!("future_unknown");
        assert!(serde_json::from_value::<StoragePreparationResult>(wrong).is_err());
        let mut extra = json;
        extra["storage_ready"] = json!(true);
        assert!(serde_json::from_value::<StoragePreparationResult>(extra).is_err());
    }

    #[test]
    fn prepared_result_rejects_empty_associations_relative_target_and_malformed_hash() {
        let json = serde_json::to_value(prepared()).unwrap();
        for (field, invalid) in [
            ("transition_id", json!("")),
            ("database_id", json!("")),
            ("target_binary", json!("relative-target")),
            ("target_sha256", json!("A".repeat(64))),
            ("target_sha256", json!("a".repeat(63))),
        ] {
            let mut bad = json.clone();
            bad[field] = invalid;
            assert!(
                serde_json::from_value::<StoragePreparationResult>(bad).is_err(),
                "{field}"
            );
        }
    }
}
