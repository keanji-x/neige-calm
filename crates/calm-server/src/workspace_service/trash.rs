//! Expired workspace trash is maintenance work, never part of an interactive
//! deletion or repoint saga. Retention and filesystem safety belong to the
//! existing workspace-runtime collector.
use std::path::PathBuf;
use std::sync::Arc;

use crate::operation::OperationRuntime;

/// No online collector is scheduled without an enforced access boundary. Keep
/// this maintenance entry explicit for callers: elapsed retention is not isolation.
pub async fn collect_once(
    workspace_root: PathBuf,
    runtime: Arc<OperationRuntime>,
    db: crate::db::sqlite::SqliteDb,
) {
    let _ = (workspace_root, runtime, db);
    tracing::debug!(
        reason = "isolation_unavailable",
        "physical workspace trash cleanup deferred"
    );
}
