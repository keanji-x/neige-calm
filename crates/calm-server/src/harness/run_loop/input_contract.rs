//! Select the actual persisted input contract before constructing the one loop.
//! Decoding establishes a format, never native readiness or execution authority.
use super::*;
use crate::harness::snapshot::CanonicalHarnessSnapshot;

#[derive(Clone, Debug)]
pub struct HarnessRuntimeSnapshot {
    pub(super) kind: RuntimeSnapshotKind,
}

#[derive(Clone, Debug)]
pub(super) enum RuntimeSnapshotKind {
    Historical(Box<HarnessSnapshot>),
    Canonical,
}

pub(super) enum LoadedRuntimeSnapshot {
    Historical(HarnessSnapshot),
    Canonical(super::canonical::CommittedFrame),
}

pub(super) enum RuntimeInputs {
    Historical(Mutex<VecDeque<QueueEntry>>),
    Canonical(Box<super::canonical::CanonicalInputs>),
}

impl RuntimeInputs {
    pub fn historical_queue(&self) -> Result<&Mutex<VecDeque<QueueEntry>>> {
        match self {
            Self::Historical(queue) => Ok(queue),
            Self::Canonical(_) => Err(CalmError::Conflict(
                "canonical inputs cannot use the historical body queue".into(),
            )),
        }
    }

    pub fn canonical(&self) -> Option<&super::canonical::CanonicalInputs> {
        match self {
            Self::Historical(_) => None,
            Self::Canonical(inputs) => Some(inputs),
        }
    }
}

impl HarnessRuntimeSnapshot {
    pub fn decode(value: Value) -> Result<Self> {
        let kind = match value.get("schema_version").and_then(Value::as_u64) {
            Some(1) => RuntimeSnapshotKind::Historical(Box::new(
                HarnessSnapshot::parse_known(value).ok_or_else(|| {
                    CalmError::Conflict("invalid historical harness snapshot".into())
                })?,
            )),
            Some(2) => {
                // The actual current frame/fence is loaded by run under SQL.
                // Retaining this caller's older pending list would make it a
                // second cache or reject a concurrent new accepted input.
                let _: CanonicalHarnessSnapshot = serde_json::from_value(value).map_err(|_| {
                    CalmError::Conflict("invalid canonical harness snapshot".into())
                })?;
                RuntimeSnapshotKind::Canonical
            }
            _ => {
                return Err(CalmError::Conflict(
                    "unsupported persisted harness input contract".into(),
                ));
            }
        };
        Ok(Self { kind })
    }
}

impl TryFrom<HarnessSnapshot> for HarnessRuntimeSnapshot {
    type Error = CalmError;

    fn try_from(snapshot: HarnessSnapshot) -> Result<Self> {
        // A typed V1 value may itself have come from untrusted serde. Do not
        // permit it to smuggle a V2 declaration into the historical queue.
        let value = serde_json::to_value(&snapshot)?;
        if !crate::harness::snapshot::is_harness_snapshot_value(&value) {
            return Err(CalmError::Conflict(
                "historical harness input requires its declared V1 contract".into(),
            ));
        }
        Ok(Self {
            kind: RuntimeSnapshotKind::Historical(Box::new(snapshot)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_load_never_falls_back_to_the_historical_queue() {
        let value = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
        let loaded = HarnessRuntimeSnapshot::decode(value.clone()).unwrap();
        assert!(matches!(loaded.kind, RuntimeSnapshotKind::Canonical));
        let mut invalid = value;
        invalid["pending_queue"] = serde_json::json!([]);
        assert!(HarnessRuntimeSnapshot::decode(invalid).is_err());
    }

    #[test]
    fn typed_historical_input_cannot_hide_a_canonical_declaration() {
        let mut value = serde_json::to_value(HarnessSnapshot::initial(7, vec![])).unwrap();
        value["schema_version"] = serde_json::json!(2);
        let historical: HarnessSnapshot = serde_json::from_value(value).unwrap();
        assert!(HarnessRuntimeSnapshot::try_from(historical).is_err());
    }
}
