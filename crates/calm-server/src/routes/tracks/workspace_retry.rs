//! Repair the receipt of a committed isolation upgrade without repointing it again.
use super::*;

pub(super) fn is_repair(old: &TrackWorkspace, requested: &TrackWorkspacePatch) -> bool {
    requested.kind == TrackWorkspaceKind::Attached
        && requested.isolation == Some(calm_types::model::WorkspaceIsolation::Worktree)
        && old.kind == TrackWorkspaceKind::Attached
        && old.worktree.is_some()
        && old.path == normalize_path(&requested.path)
        && crate::workspace_service::validate_runtime_workspace(old).is_err()
}

/// Admit an already selected configuration in the same writer.
/// Keep both lifecycle guards through filesystem repair, including when the receipt is valid.
pub(crate) async fn admit_and_repair(
    s: &RouteState,
    track: &Track,
    requested: &TrackWorkspacePatch,
) -> Result<(Track, bool)> {
    require_user_workspace_area(s, track).await?;
    let operation_guard = s.operation_runtime.lock_for_track_delete().await;
    let held = crate::workspace_service::HeldTrack::new(
        crate::per_card_lock::lock_key(&s.track_delete_locks, track.id.as_str()).await,
    );
    let id = track.id.clone();
    let requested = requested.clone();
    let teardown_targets = s.teardown_targets.clone();
    let (live, active_runtime) = crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move { admit_tx(tx, &id, &requested, &teardown_targets).await })
    })
    .await?;
    if live.workspace.worktree.is_some()
        && crate::workspace_service::validate_runtime_workspace(&live.workspace).is_err()
    {
        crate::workspace_service::materialize_configuration(s, &live, Some(held.clone())).await?;
    }
    held.release();
    drop(operation_guard);
    Ok((live, active_runtime))
}

/// Both writer paths use the same authoritative repair admission.
async fn admit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &TrackId,
    requested: &TrackWorkspacePatch,
    teardown_targets: &workspace_runtime::sweep::TeardownTargets,
) -> Result<(Track, bool)> {
    crate::workspace_service::refuse_admitted_teardown(teardown_targets, id.as_str())?;
    let live = track_get_tx(tx, id).await?;
    if live.workspace.path != normalize_path(&requested.path)
        || Some(live.workspace.isolation()) != requested.isolation
    {
        return Err(CalmError::Conflict(
            "workspace selection changed before repair".into(),
        ));
    }
    crate::operation::terminal_disposal::require_track_location_safe_tx(tx, id.as_str()).await?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE track_id=? AND status IN ('dispatched','running','verifying'))",
    ).bind(id.as_str()).fetch_one(&mut **tx).await?;
    if active {
        return Err(CalmError::Conflict(
            "workspace repair requires settled tasks".into(),
        ));
    }
    let active_runtime: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE track_id=? AND state IN ('starting','running','idle','turn_pending'))",
    ).bind(id.as_str()).fetch_one(&mut **tx).await?;
    Ok((live, active_runtime))
}

/// Same-locator upgrades preserve every running Planner and its original native directory.
pub(super) async fn upgrade_same_locator(
    state: &RouteState,
    track: &Track,
    plan: &TrackWorkspacePlan,
) -> Result<Track> {
    let TrackWorkspacePlan::AttachedWithTrackWorktree(root) = plan else {
        return Err(CalmError::Internal(
            "isolation upgrade lost its repository".into(),
        ));
    };
    let _operation_guard = state.operation_runtime.lock_for_track_delete().await;
    let held = crate::workspace_service::HeldTrack::new(
        crate::per_card_lock::lock_key(&state.track_delete_locks, track.id.as_str()).await,
    );
    let selected = track.clone();
    let root = root.clone();
    let roots = state.owned_location_roots();
    let targets = state.teardown_targets.clone();
    let (updated, _) = write_with_actor_events_typed(
        state.repo.as_ref(),
        None,
        &state.events,
        &state.write,
        move |tx| {
            Box::pin(async move {
                validate_workspace_upgrade_tx(
                    tx,
                    selected.id.as_str(),
                    &selected.workspace.path,
                    true,
                    true,
                )
                .await?;
                let updated = crate::workspace_service::isolation::select_worktree_tx(
                    tx, &selected, &root, &roots, &targets,
                )
                .await?;
                let scope = EventScope::Track {
                    track: updated.id.clone(),
                    area: updated.area_id.clone(),
                };
                let event = Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                    updated.clone(),
                    None,
                ));
                Ok((updated, vec![(ActorId::User, scope, event)]))
            })
        },
    )
    .await?;
    crate::workspace_service::materialize_configuration(state, &updated, Some(held.clone()))
        .await?;
    held.release();
    Ok(updated)
}
