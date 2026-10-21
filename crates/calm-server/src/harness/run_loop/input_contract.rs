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
    /// Advisory eligibility to seek the track fence; the canonical claim and
    /// native admission still recheck the committed receipt and exact activity.
    pub(super) async fn can_seek_issuance(&self, inner: &Inner) -> Result<bool> {
        if inner.state.lock().await.can_issue_turn() {
            return Ok(true);
        }
        match self {
            Self::Historical(_) => Ok(false),
            Self::Canonical(_) => Ok(super::canonical::read_current(inner)
                .await?
                .snapshot
                .canonical_inputs
                .resolved_execution_without_target()),
        }
    }

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

    pub(super) async fn idle_for_lifecycle(&self, inner: &Inner) -> Result<bool> {
        match self {
            Self::Historical(queue) => Ok(queue.lock().await.is_empty()),
            Self::Canonical(_) => Ok(canonical_idle_for_lifecycle(
                &super::canonical::read_current(inner).await?.snapshot,
            )),
        }
    }

    pub(super) async fn checkpoint_for_lifecycle(&self, inner: &Arc<Inner>) -> Result<bool> {
        match self {
            Self::Historical(_) => {
                super::settle_owner_input_before_release(inner).await?;
                super::persist_owned_input_snapshot(inner).await?;
                Ok(true)
            }
            // Canonical input and runtime facts are already committed by their
            // owning transitions. Re-read after the lifecycle pause; never send
            // this frame through the historical snapshot writer.
            Self::Canonical(_) => self.idle_for_lifecycle(inner).await,
        }
    }
}

impl HarnessRuntimeSnapshot {
    /// Persisted sleep eligibility is interpreted by the same input owner as
    /// live quiescence. This is not permission to restore or execute a thread.
    pub(crate) fn idle_for_lifecycle(value: Value) -> Result<bool> {
        match Self::decode(value.clone())?.kind {
            RuntimeSnapshotKind::Historical(_) => {
                // Preserve the published V1 scan's exact queue prerequisite,
                // including not treating a missing body array as empty.
                Ok(value
                    .get("pending_queue")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty))
            }
            RuntimeSnapshotKind::Canonical => Ok(canonical_idle_for_lifecycle(
                &serde_json::from_value(value)?,
            )),
        }
    }

    pub fn decode(value: Value) -> Result<Self> {
        let kind = match value.get("schema_version").and_then(Value::as_u64) {
            Some(1) => RuntimeSnapshotKind::Historical(Box::new(
                HarnessSnapshot::parse_known(value).ok_or_else(|| {
                    CalmError::Conflict("invalid historical harness snapshot".into())
                })?,
            )),
            Some(version) if version >= 2 => {
                // The actual current frame/fence is loaded by run under SQL.
                // Retaining this caller's older pending list would make it a
                // second cache or reject a concurrent new accepted input.
                let _: CanonicalHarnessSnapshot = serde_json::from_value(value).map_err(|_| {
                    CalmError::Conflict("invalid canonical harness snapshot".into())
                })?;
                RuntimeSnapshotKind::Canonical
            }
            Some(_) | None => {
                return Err(CalmError::Conflict(
                    "unsupported persisted harness input contract".into(),
                ));
            }
        };
        Ok(Self { kind })
    }
}

fn canonical_idle_for_lifecycle(snapshot: &CanonicalHarnessSnapshot) -> bool {
    snapshot.idle_for_lifecycle()
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
