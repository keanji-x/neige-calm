//! Recovery-specific admission uses frozen evidence, not a second author plan.
use std::collections::BTreeMap;

use calm_types::report_blocks::tasks::{Diagnostic, TaskDeclaration};
use calm_types::task_recovery::{
    TaskAttemptOrigin, TaskRecoveryConstraint, task_root_hash_preimage,
};
use calm_types::track_report::ReportBlock;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::task_projection::BlockVerdict;
use crate::error::{CalmError, Result};

#[derive(Deserialize)]
pub(super) struct RecoveryProjection {
    key: String,
    origin: TaskAttemptOrigin,
}

/// Facts are materialized with all other projection facts in one SQL snapshot.
/// Root bytes come from the report payload cache, which every production report
/// writer updates before projection. Reconstructing a raw root from normalized
/// TaskDeclaration would change hashes for absent versus explicitly empty fields.
pub(super) fn constrain_recovery_declarations(
    track_id: &str,
    declarations: &[TaskDeclaration],
    verdicts: &mut [BlockVerdict],
    recoveries: &[RecoveryProjection],
    blocks: &[serde_json::Value],
) -> Result<()> {
    for recovery in recoveries {
        let TaskAttemptOrigin::Recovery { constraint, .. } = &recovery.origin else {
            return Err(CalmError::Internal(
                "recovery projection contained initial allocation".into(),
            ));
        };
        constraint.validate(track_id).map_err(CalmError::Internal)?;
        let TaskRecoveryConstraint::V1 {
            refs,
            spawn,
            declared_by,
        } = constraint;
        let root = refs
            .iter()
            .find(|reference| reference.is_root)
            .expect("validated root");
        let live: Vec<ReportBlock> = blocks
            .iter()
            .filter_map(|value| serde_json::from_value::<ReportBlock>(value.clone()).ok())
            .filter(|block| {
                block.kind == "task"
                    && block.payload.get("key").and_then(serde_json::Value::as_str)
                        == Some(&recovery.key)
                    && block
                        .payload
                        .get("tombstone")
                        .is_none_or(serde_json::Value::is_null)
            })
            .collect();
        for (declaration, verdict) in declarations.iter().zip(verdicts.iter_mut()) {
            if declaration.key != recovery.key || declaration.tombstone {
                continue;
            }
            let unchanged = match live.as_slice() {
                [block] => {
                    block.id == declaration.block_id
                        && declaration.spawn == *spawn
                        && declaration.declared_by == *declared_by
                        && format!(
                            "{:x}",
                            Sha256::digest(task_root_hash_preimage(&block.payload))
                        ) == root.hash
                }
                _ => false,
            };
            if !unchanged {
                verdict.schedulable = false;
                if !verdict
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "declaration_changed_in_flight")
                {
                    verdict.diagnostics.push(Diagnostic::coded(
                        "declaration_changed_in_flight",
                        "key",
                        BTreeMap::new(),
                        vec![],
                        None,
                        Some("open_worker_output".into()),
                    ));
                }
            }
        }
    }
    Ok(())
}
