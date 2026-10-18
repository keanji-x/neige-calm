//! Kernel-owned binding between one collected observation and its accepted operation claim.
use super::{Operation, required_lease_owner};
use crate::error::{CalmError, Result};

pub(crate) struct PreparationClaim {
    original: Operation,
    owner: String,
}
impl PreparationClaim {
    pub(crate) fn new(op: &Operation) -> Result<Self> {
        Ok(Self {
            original: op.clone(),
            owner: required_lease_owner(op)?.to_owned(),
        })
    }
    pub(crate) fn require_operation(&self, op: &Operation) -> Result<()> {
        let original = &self.original;
        if original.id != op.id
            || original.operation_key != op.operation_key
            || original.kind != op.kind
            || original.idempotency_key != op.idempotency_key
            || original.payload_hash != op.payload_hash
            || original.payload != op.payload
            || original.phase != op.phase
            || self.owner != required_lease_owner(op)?
        {
            return Err(CalmError::Internal(
                "prepared evidence belongs to a different operation claim".into(),
            ));
        }
        Ok(())
    }
}
