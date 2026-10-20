//! Closed creation-time capabilities. Adding a variant also requires a storage CHECK migration.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackCapability {
    Objectives,
}

impl TrackCapability {
    pub const ALL: &'static [Self] = &[Self::Objectives];

    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Objectives => "objectives",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_wire_and_storage_vocabulary_agree() {
        for &capability in TrackCapability::ALL {
            let wire = serde_json::to_value(capability).unwrap();
            assert_eq!(wire, capability.as_db_str());
            assert_eq!(
                serde_json::from_value::<TrackCapability>(wire).unwrap(),
                capability
            );
        }
        assert!(serde_json::from_str::<TrackCapability>("\"unknown\"").is_err());
    }
}
