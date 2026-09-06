//! Recovery-specific admission uses frozen evidence, not a second author plan.
use std::collections::BTreeMap;

use calm_types::report_blocks::tasks::{Diagnostic, TaskDeclaration, TaskDeclarationSource};
use calm_types::task_recovery::{TaskAttemptOrigin, TaskRecoveryConstraint};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::task_projection::BlockVerdict;
use crate::error::{CalmError, Result};

#[derive(Deserialize)]
pub(super) struct RecoveryProjection {
    key: String,
    origin: TaskAttemptOrigin,
}

/// Allocation facts share the projection's DB snapshot. Root evidence belongs
/// to each declaration and comes from its original report snapshot, which may be
/// authoritative CRDT bytes that have not been mirrored to the JSON cache.
pub(super) fn constrain_recovery_declarations(
    track_id: &str,
    declarations: &[TaskDeclaration],
    verdicts: &mut [BlockVerdict],
    recoveries: &[RecoveryProjection],
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
        let unique_live_declaration = declarations
            .iter()
            .filter(|declaration| declaration.key == recovery.key && !declaration.tombstone)
            .take(2)
            .count()
            == 1;
        for (declaration, verdict) in declarations.iter().zip(verdicts.iter_mut()) {
            if declaration.key != recovery.key || declaration.tombstone {
                continue;
            }
            let unchanged = match &declaration.source {
                TaskDeclarationSource::Report { root_hash_preimage } => {
                    unique_live_declaration
                        && declaration.block_id == root.block_id
                        && declaration.spawn == *spawn
                        && declaration.declared_by == *declared_by
                        && format!("{:x}", Sha256::digest(root_hash_preimage.as_bytes()))
                            == root.hash
                }
                TaskDeclarationSource::ValidationOnly => false,
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
