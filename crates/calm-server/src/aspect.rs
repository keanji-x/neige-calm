//! Aspect / join-point framework shell.
//!
//! D5 retired the legacy spec-push handle parking invariant. The registry
//! stays as the stable state-owned extension point for future kernel
//! invariants, but it currently has no installed join points.

#[derive(Default)]
pub struct AspectRegistry;

impl AspectRegistry {
    pub fn new() -> Self {
        Self
    }
}
