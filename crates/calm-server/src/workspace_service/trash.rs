//! Expired workspace trash is maintenance work, never part of an interactive
//! deletion or repoint saga. Retention and filesystem safety belong to the
//! existing workspace-runtime collector.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::FromRef;

use crate::operation::OperationRuntime;
use crate::state::{AppState, RouteState};

const COLLECTION_PERIOD: Duration = Duration::from_secs(10 * 60);

/// One maintenance pass through the production collector. Sample its fixed time
/// after any existing destructive saga completes, then release the lifecycle
/// fence before filesystem work. A later saga's newly recycled directory is
/// younger than this pass's retention cutoff and remains available for rollback.
pub async fn collect_once(
    workspace_root: PathBuf,
    runtime: Arc<OperationRuntime>,
    db: crate::db::sqlite::SqliteDb,
) {
    let started = Instant::now();
    let (now, retained) = {
        let _fence = runtime.lock_for_track_delete().await;
        let retained =
            calm_truth::db::sqlite::track_workspace_trash_retention_owners(db.read_pool()).await;
        let retained = match retained {
            Ok(ids) => ids.into_iter().collect(),
            Err(error) => {
                tracing::warn!(%error, "trash gc: retention inventory failed; skipping pass");
                return;
            }
        };
        (crate::model::now_ms(), retained)
    };
    let collected = tokio::task::spawn_blocking(move || {
        super::ports::observe("workspace-trash-collection", &workspace_root);
        workspace_runtime::recycle::gc_trash_best_effort(&workspace_root, now, &retained);
    })
    .await;
    match collected {
        Ok(()) => tracing::info!(
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            outcome = "finished_best_effort",
            "expired workspace trash maintenance finished"
        ),
        Err(error) => tracing::warn!(%error, "expired workspace trash maintenance task failed"),
    }
}

/// One serial pass at boot and every ten minutes. Missed ticks are skipped;
/// slow filesystem work never creates overlapping or queued collection passes.
pub fn spawn(state: &AppState) -> tokio::task::JoinHandle<()> {
    let route = RouteState::from_ref(state);
    let root = route.workspace_root;
    let runtime = route.operation_runtime;
    let db = state.sqlite_db();
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(COLLECTION_PERIOD);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Some(db) = &db {
                collect_once(root.clone(), runtime.clone(), db.clone()).await;
            }
        }
    })
}
