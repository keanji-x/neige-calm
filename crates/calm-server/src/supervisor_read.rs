//! Read-only composition of observations owned by runtime components.
use crate::state::RouteState;
use calm_truth::db::sqlite::{WRITE_BUCKET_MS, WRITE_BUCKETS, WriteObservationBucket};
use chrono::Utc;
use serde::Serialize;
use std::time::Duration;
use utoipa::ToSchema;

/// A current observation, with null values for unavailable sources.
#[derive(Serialize, ToSchema)]
pub(crate) struct SupervisorSnapshot {
    observed_at_ms: i64,
    kernel_version: String,
    #[schema(required = true)]
    build_sha: Option<String>,
    #[schema(required = true)]
    database_writes: Option<Vec<WriteObservationBucket>>,
    write_bucket_ms: i64,
    write_bucket_count: i64,
    scheduler_boot_sweep_completed: bool,
    #[schema(required = true)]
    settled_worker_cleanup_candidates: Option<i64>,
    #[schema(required = true)]
    retired_workspaces: Option<RetiredWorkspaceSummary>,
}

#[derive(Serialize, ToSchema)]
struct RetiredWorkspaceSummary {
    count: i64,
    #[schema(required = true)]
    oldest_retired_at_ms: Option<i64>,
}

pub(crate) async fn snapshot(state: &RouteState) -> SupervisorSnapshot {
    let version = crate::routes::version::current_version_info(
        (*state.db_instance_id).clone(),
        (*state.database_id).clone(),
    );
    let mut snapshot = SupervisorSnapshot {
        observed_at_ms: Utc::now().timestamp_millis(),
        kernel_version: version.kernel_version,
        build_sha: version.build_sha,
        database_writes: None,
        write_bucket_ms: WRITE_BUCKET_MS,
        write_bucket_count: WRITE_BUCKETS,
        scheduler_boot_sweep_completed: state.dispatcher.scheduler().boot_sweep_completed(),
        retired_workspaces: None,
        settled_worker_cleanup_candidates: None,
    };
    if let Some(db) = state.mcp_context.sqlite_db.as_ref() {
        snapshot.database_writes = Some(db.writer().recent_observations(snapshot.observed_at_ms));
        let retirement = tokio::time::timeout(Duration::from_secs(1), async {
            let mut conn = db.read_pool().acquire().await?;
            calm_truth::db::sqlite::workspace_retirement::retirement_summary(&mut conn).await
        });
        let candidates = tokio::time::timeout(
            Duration::from_secs(1),
            crate::terminal_sweeper::settled_worker_cleanup_count(db),
        );
        let (retirement, candidates) = tokio::join!(retirement, candidates);
        snapshot.settled_worker_cleanup_candidates = candidates.ok().and_then(Result::ok);
        snapshot.retired_workspaces = retirement.ok().and_then(std::result::Result::ok).map(
            |(count, oldest_retired_at_ms)| RetiredWorkspaceSummary {
                count,
                oldest_retired_at_ms,
            },
        );
    }
    snapshot
}
