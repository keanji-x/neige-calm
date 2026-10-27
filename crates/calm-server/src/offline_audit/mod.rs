//! Read-only offline filesystem facts. Callers independently establish frozen-source
//! and ownership evidence; observations do not certify quiescence or export eligibility.
pub use crate::planner_attachments::export_read::{
    Diagnostic, EntryKind, Identity, PartialRead, RawEntry, inventory_owned_tree,
};
pub use crate::routes::fs::TrustedReadRoot;

#[cfg(test)]
mod inventory_tests;
#[cfg(test)]
mod root_tests;

#[cfg(test)]
mod read_error_tests;
