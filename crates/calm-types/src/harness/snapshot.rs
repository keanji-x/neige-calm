//! IO-free values carried by the persisted harness snapshot; the codec stays in the server.
mod values;
pub use values::{
    CompactOutcome, CompactStatus, PersistedRewind, QueueEntryId, TokenUsage, TurnBase,
};
