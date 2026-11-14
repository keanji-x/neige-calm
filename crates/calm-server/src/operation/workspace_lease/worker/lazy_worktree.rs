//! Same-locator isolation upgrade. Original Planner carriers and native cwd stay bound.
use super::*;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::operation::{Operation, SpawnCtx};
use crate::workspace_service::{HeldTrack, refuse_admitted_teardown};

pub(super) async fn prepare(
    ctx: &SpawnCtx,
    op: &Operation,
    track_id: &str,
    attempt: &str,
) -> Result<()> {
    let track = ctx
        .repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("Track".into()))?;
    if track.closed_at.is_some() || track.workspace.kind != TrackWorkspaceKind::Attached {
        return Ok(());
    }
    if track.workspace.worktree.is_some()
        && crate::workspace_service::validate_runtime_workspace(&track.workspace).is_ok()
    {
        return Ok(());
    }
    if track.workspace.worktree.is_none()
        && workspace_runtime::selection::git_repo_root_for_track_cwd(
            track_id,
            &track.workspace.path,
        )
        .is_err()
    {
        return Ok(());
    }
    // Driver preparation normally owns this guard; direct callers acquire the same lock.
    let held = match ctx.held_track() {
        Ok(held) => held,
        Err(_) => {
            HeldTrack::new(crate::per_card_lock::lock_key(&ctx.track_delete_locks, track_id).await)
        }
    };
    let _guard = held.require_held(track_id)?;
    let admission = ctx
        .location_admission
        .as_ref()
        .ok_or_else(|| CalmError::Internal("location admission is not configured".into()))?;
    refuse_admitted_teardown(&admission.teardown_targets, track_id)?;
    let pool = ctx.operation_repo.sqlite_db();
    let selected = ctx
        .repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("Track".into()))?;
    if selected.closed_at.is_some() || selected.workspace.kind != TrackWorkspaceKind::Attached {
        return Ok(());
    }
    let selected = if selected.workspace.worktree.is_none() {
        let repo_root = match workspace_runtime::selection::git_repo_root_for_track_cwd(
            track_id,
            &selected.workspace.path,
        ) {
            Ok(root) => std::fs::canonicalize(root)?,
            Err(_) => return Ok(()), // A non-Git locator still gets track_without_worktree.
        };
        use super::super::upstream::{LeaseStart, choose_lease_start, diverged_refusal};
        if let LeaseStart::Diverged {
            head,
            upstream,
            unpushed,
            ahead,
            behind,
        } = choose_lease_start(&repo_root)?
        {
            return Err(diverged_refusal(
                &repo_root, &head, &upstream, unpushed, ahead, behind,
            ));
        }
        let (track, events) = pool
            .writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await?;
                    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                    crate::operation::refuse_if_context_stale(&mut tx, Some(attempt)).await?;
                    refuse_admitted_teardown(&admission.teardown_targets, track_id)?;
                    let track = crate::workspace_service::isolation::select_worktree_tx(
                        &mut tx,
                        &selected,
                        &repo_root,
                        &[
                            admission.managed_root.clone(),
                            admission.objective_checkout_root.clone(),
                        ],
                        &admission.teardown_targets,
                    )
                    .await?;
                    let events = append_workspace_events_tx(
                        &mut tx,
                        vec![(
                            ActorId::Kernel,
                            EventScope::Track {
                                track: track.id.clone(),
                                area: track.area_id.clone(),
                            },
                            Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                                track.clone(),
                                None,
                            )),
                        )],
                    )
                    .await?;
                    tx.commit().await?;
                    Ok::<_, CalmError>((track, events))
                })
            })
            .await
            .map_err(calm_truth::TruthError::from)??;
        for event in events {
            ctx.events.emit_envelope(event);
        }
        track
    } else {
        selected // Retry repairs the persisted selection after an interrupted first materialization.
    };
    let track_id = track_id.to_owned();
    let attempt = attempt.to_owned();
    let op = op.clone();
    let targets = admission.teardown_targets.clone();
    crate::workspace_service::retained_task_worktree(
        ctx.repo.clone(),
        pool.clone(),
        &selected,
        held.clone(),
        move || {
            let held = held.clone();
            let track_id = track_id.clone();
            let attempt = attempt.clone();
            let op = op.clone();
            let targets = targets.clone();
            let pool = pool.clone();
            async move {
                held.require_held(&track_id)?;
                refuse_admitted_teardown(&targets, &track_id)?;
                pool.writer()
                    .with_writer(|session| {
                        Box::pin(async move {
                            let mut tx = session.begin_immediate().await?;
                            crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, &op)
                                .await?;
                            crate::task_recovery::require_checkout_startable_tx(&mut tx, &attempt)
                                .await?;
                            tx.rollback().await?;
                            Ok::<_, CalmError>(())
                        })
                    })
                    .await
                    .map_err(calm_truth::TruthError::from)?
            }
        },
    )
    .await
}
