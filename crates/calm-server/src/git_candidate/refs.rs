//! Candidate and gate-run reference identities. Logical retirement retains these
//! references; the metadata below is not permission for online physical cleanup.

/// The prefix every candidate ref of one Track shares (the ref is
/// `refs/neige/candidates/<track>/<card>/<delivery_id>`, `delivery::candidate_ref_name`).
pub(crate) fn candidate_ref_prefix(track_id: &str) -> String {
    format!("refs/neige/candidates/{track_id}/")
}

/// The same declared prefixes drive deletion and HEAD retention checks during workspace sweep.
pub(crate) fn teardown_ref_prefixes(track_id: &str) -> [String; 2] {
    [
        candidate_ref_prefix(track_id),
        crate::operation::task_gate_run::gate_run_ref_prefix(track_id),
    ]
}
