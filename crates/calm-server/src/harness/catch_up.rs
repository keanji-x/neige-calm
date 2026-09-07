//! Persisted Planner push observations shared by boot replay and settlement catch-up.
use super::Observation;
use crate::db::Repo;
use crate::dispatcher;
use crate::error::Result;
use crate::event::Event;
use crate::ids::TrackId;
use crate::model::CardRole;

/// `through` bounds delivery to an already committed envelope. The existing
/// Track event reader orders by ID; never select a later event for this prefix.
pub(crate) async fn observations_since(
    repo: &dyn Repo,
    track_id: &TrackId,
    watermark: i64,
    through: Option<i64>,
) -> Result<Vec<(i64, Observation)>> {
    let rows = repo
        .events_for_track(
            track_id.as_str(),
            &[
                "task.completed",
                "task.failed",
                "task.execution_settled",
                "task.file_publication_settled",
                // Issue #644 PR-C (§6.5/§8) — gate verdicts that
                // landed while the kernel was down replay like live
                // pushes.
                "task.gate_result",
                "track.report_edited",
                "workspace.leased",
                "workspace.released",
                "forge.scan.completed",
                "forge.pr.opened",
                "forge.pr.checks",
                "forge.issue.closed",
                "worktree.provisioned",
                "worktree.committed",
                "forge.pr.merged",
                "review.round",
                "ratify.requested",
                "ratify.resolved",
                "codex.hook",
                "claude.hook",
            ],
            Some(watermark),
        )
        .await?;
    let mut observations = Vec::new();
    for row in rows {
        if through.is_some_and(|last| row.id > last) {
            break;
        }
        let role = role_needed_for_planner_push_filter(repo, &row.event).await?;
        if !dispatcher::event_warrants_planner_push_with_role(&row.event, &row.actor, |_| role) {
            continue;
        }
        // Issue #644 PR-C (§6.5) — the SAME gated-self-report
        // consultation the live push branch runs: a crash between the
        // emit tx and the live push must not replay a gated task's
        // raw self-report to the planner.
        if dispatcher::is_gated_self_report(repo, &row.event).await {
            continue;
        }
        let Some(obs) = dispatcher::resolve_harness_observation(repo, track_id, &row.event).await?
        else {
            continue;
        };
        observations.push((row.id, obs));
    }
    Ok(observations)
}

async fn role_needed_for_planner_push_filter(
    repo: &dyn Repo,
    event: &Event,
) -> Result<Option<CardRole>> {
    match event {
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => repo
            .card_role_get(card_id.as_str())
            .await
            .map_err(Into::into),
        _ => Ok(None),
    }
}
