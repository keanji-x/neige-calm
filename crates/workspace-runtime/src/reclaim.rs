//! Frozen extra-worktree identity retained for legacy reclaim recovery.
use workspace::reclaim::ReclaimRequest;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtraWorktree {
    pub operation_id: String,
    pub provision_id: i64,
    pub track_id: String,
    pub card_id: String,
    pub area_id: String,
    pub request: ReclaimRequest,
}
