//! The Track structure allocation step: everything the track-create transaction writes.
//! Worktree materialization stays with the caller, after commit.
use super::*;
use crate::state::WriteContext;
use sqlx::{Sqlite, Transaction};
use std::sync::{Arc, OnceLock};

/// The admitted entry decides where its frozen material is consumed.
pub(super) enum CreationContext {
    Historical,
    /// Trusted managed admission; no Human input or borrowed resource authority.
    ManagedCanonical,
    Canonical(crate::auth::HumanInputSource),
}

/// The route-state owners the allocation reads inside the transaction.
pub(super) struct TrackAllocationOwners {
    pub(super) context: CreationContext,
    pub(super) acp_planner: Arc<crate::acp_planner::config::AcpPlannerHost>,
    pub(super) write: WriteContext,
    pub(super) templates: &'static TemplateRoster,
    pub(super) teardown_targets: workspace_runtime::sweep::TeardownTargets,
    pub(super) admission_root: std::path::PathBuf,
}

pub(super) type TrackAllocationWrite = ((Track, bool), Vec<(ActorId, EventScope, Event)>);

#[allow(deprecated)]
pub(super) async fn allocate_track_structure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    actor_id_for_tx: ActorId,
    p: NewTrack,
    options: CreateTrackOptions,
    (planner_card_id_for_tx, report_card_id_for_tx): (String, String),
    owners: TrackAllocationOwners,
    replay_tx: Arc<OnceLock<String>>,
) -> Result<TrackAllocationWrite> {
    let CreateTrackOptions {
        human_source,
        admin,
        managed_identity,
        creator,
        creation_message,
        planner_provider,
        model,
        reasoning_effort,
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
        idempotency_claim,
    } = options;
    if let Some(source) = &human_source {
        source.require_area_tx(tx, &body_area_id).await?;
        if let TrackInit::Fork { source_track_id } = &init {
            source
                .require_track_tx(tx, source_track_id)
                .await
                .map_err(|error| fork_source_error(error, source_track_id))?;
        }
        if let FolderClaim::Enforce {
            allow_cross_area_cwd: Some(cross),
            ..
        } = &folder_claim
        {
            source.require_area_tx(tx, cross.area_id.as_str()).await?;
        }
    }
    if let CreationContext::Canonical(source) = &owners.context
        && !matches!(workspace_plan, TrackWorkspacePlan::ManagedUnder(_))
    {
        let source_area = match &folder_claim {
            FolderClaim::Enforce {
                allow_cross_area_cwd: Some(cross),
                ..
            } => Some(cross.area_id.as_str()),
            _ => None,
        };
        crate::workspace_service::require_borrowed_admission_tx(
            tx,
            source,
            &body_area_id,
            source_area,
        )
        .await?;
    }
    let TrackAllocationOwners {
        context,
        acp_planner,
        write: write_for_tx,
        templates,
        teardown_targets,
        admission_root,
    } = owners;
    let area_id_for_attach = body_area_id;
    let normalized_cwd_for_tx = normalized_cwd;
    let idempotency_claim_for_tx = idempotency_claim;
    if let Some(identity) = &managed_identity {
        let existing: Option<crate::managed_track::ManagedTrackBinding> = sqlx::query_as(concat!(
"SELECT ",
"m.track_id,t.area_id,m.report_read_scope,m.report_time_zone,m.tool_policy,m.kernel_controls_lifecycle,t.template_id",
" FROM managed_track_identities m JOIN tracks t ON t.id=m.track_id WHERE m.owner=?1 AND ",
"m.identity=?2",
))
            .bind(&identity.owner).bind(&identity.identity).fetch_optional(&mut **tx).await?;
        if let Some(binding) = existing {
            if !binding.matches(identity, p.area_id.as_str(), &p.template_id) {
                return Err(CalmError::Conflict(
                    "managed Track identity belongs to different creation metadata".into(),
                ));
            }
            let _ = replay_tx.set(binding.track_id);
            return Err(CalmError::Conflict("managed Track already exists".into()));
        }
    }
    // Counted in this transaction, so two concurrent adds cannot both take the last slot.
    if let Some(creator) = &creator {
        creator.admit_tx(tx).await?;
    }
    calm_truth::db::sqlite::admit_attached_workspace_tx(
        tx,
        &p.cwd,
        &admission_root,
        &teardown_targets.snapshot(),
        &workspace_plan,
    )
    .await?;
    // Claim scan + insert is atomic with the row, after ownership admission and
    // before any row writes, so every branch leaves the claim table consistent.
    enforce_folder_claim_tx(
        tx,
        &folder_claim,
        &area_id_for_attach,
        &normalized_cwd_for_tx,
        FolderClaimIntent::Create,
        FolderClaimPass::Authoritative,
    )
    .await?;

    // The recipe is read inside the transaction, once, before the INSERT: its `revision` is
    // stamped onto the row, and reading twice would let a concurrent edit split the
    // recorded revision from the report.
    let recipe_source = match &init {
        TrackInit::Recipe { recipe_id } => {
            Some(track_recipe_get_tx(tx, recipe_id).await?.ok_or_else(|| {
                CalmError::BadRequest(format!("track create: recipe `{recipe_id}` does not exist"))
            })?)
        }
        _ => None,
    };
    let recipe_origin = recipe_source.as_ref().map(|recipe| TrackRecipeOrigin {
        recipe_id: recipe.id.clone(),
        revision: recipe.revision,
    });

    let mut track = track_create_tx(
        tx,
        p,
        None,
        &workspace_plan,
        recipe_origin.as_ref(),
        write_for_tx.area_cache(),
    )
    .await?;
    if let Some(creator) = &creator {
        creator.stamp_tx(tx, &mut track).await?;
    }
    let track_id = track.id.clone();
    let area_id = track.area_id.clone();
    if let Some(identity) = &managed_identity {
        crate::managed_track::bind_tx(tx, identity, track_id.as_str()).await?;
    }

    if let TrackInit::Template { capabilities, .. } = &init {
        if !capabilities.is_empty()
            && managed_identity.as_ref().is_some_and(|identity| {
                matches!(
                    identity.tool_policy,
                    crate::managed_track::ToolPolicy::Reports
                )
            })
        {
            return Err(CalmError::Internal(
                "reports-only managed Track cannot declare capabilities".into(),
            ));
        }
        calm_truth::db::sqlite::track_capabilities::stamp_tx(tx, &track_id, capabilities).await?;
    }

    if admin {
        track = crate::area_admin::set_tx(tx, &track, true).await?;
    }

    // The `Idempotency-Key` → track binding is written in the same transaction that mints
    // the id; the `operations` row cannot carry it (written after validation on a pooled connection).
    if let Some(claim) = idempotency_claim_for_tx.as_ref() {
        track_create_idempotency_claim_tx(
            tx,
            area_id.as_str(),
            &claim.key,
            &calm_truth::db::sqlite::TrackCreateBindingClaim {
                track_id: track_id.to_string(),
                planner_card_id: planner_card_id_for_tx.clone(),
                report_card_id: report_card_id_for_tx.clone(),
                create_request_sha256: claim.create_request_sha256.clone(),
                first_message_sha256: claim.first_message_sha256.clone(),
            },
        )
        // A primary-key violation is the cross-instance racer: 409
        // `idempotency_key_concurrent`, and its retry resolves to `Resume`. Not recovered
        // in place because this transaction already wrote a track row it must not keep.
        .await?;
    }

    // Matched on `(&init, recipe_source)` as one value so the `Recipe` arm binds its
    // recipe by pattern.
    let init_snapshot = match (&init, recipe_source) {
        (TrackInit::Blank, _) => None,
        (TrackInit::Template { key, .. }, _) => Some(prepare_template_report(templates, key)?),
        (TrackInit::Recipe { recipe_id }, None) => {
            // Not a caller error, so not a 400.
            return Err(CalmError::Internal(format!(
                "track create: recipe `{recipe_id}` was resolved to a Recipe init \
                 without the recipe row the same `init` was supposed to read"
            )));
        }
        (TrackInit::Recipe { recipe_id }, Some(recipe)) => {
            // The stored body is already normalized by the write boundary; nothing is re-normalized here.
            Some(prepare_initial_report_payload(
                recipe_id,
                TrackReportPayload::new(recipe.title, recipe.body),
            )?)
        }
        (TrackInit::Fork { source_track_id }, _) => {
            // A fork records no recipe provenance, even when the source was recipe-born: a fork was
            // instantiated from a track, not from a recipe.
            let source_track_id = source_track_id.as_str();
            let source_id = TrackId::from(source_track_id.to_string());
            let source_track = track_get_tx(tx, &source_id)
                .await
                .map_err(|error| fork_source_error(error.into(), source_track_id))?;
            // A Report copy admits content, never the source's execution grant.
            if let CreationContext::Canonical(source) = &context {
                crate::organization_access::require_area_access_tx(
                    tx,
                    source_track.area_id.as_str(),
                    &source.current_principal()?,
                    calm_types::organization::AreaAccess::Collaborate,
                )
                .await?;
            }
            let source_area_kind: String = sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                .bind(source_track.area_id.as_str())
                .fetch_one(&mut **tx)
                .await?;
            if source_track.area_id != area_id && source_area_kind != AreaKind::System.as_db_str() {
                return Err(CalmError::BadRequest(format!(
                    "track create: fork source track `{source_track_id}` must be in the target area or the system area"
                )));
            }
            let (summary, blocks) = report_blocks_snapshot_tx(tx, source_track_id).await?;
            // The resolved `chart.series` rows travel with the report (block ids survive the fork).
            // Pinned rows stay immutable in the child; unpinned rows refresh per track from here on.
            crate::report_series::store::copy_rows_tx(tx, source_track_id, track_id.as_str())
                .await?;
            // The captured sources travel too, verbatim, so the child resolves them independently
            // of the parent's lifetime.
            crate::report_sources::store::copy_rows_tx(tx, source_track_id, track_id.as_str())
                .await?;
            // The report's tags describe the copied content, so they travel too, in order.
            crate::report_tags::store::copy_rows_tx(tx, source_track_id, track_id.as_str()).await?;
            Some(prepare_fork_report(
                summary,
                blocks,
                source_track_id,
                track_id.as_str(),
            )?)
        }
    };

    let mut planner_payload = planner_card_payload(
        &acp_planner,
        None,
        planner_provider.clone(),
        init_snapshot.as_ref(),
    )?;
    if matches!(
        context,
        CreationContext::Canonical(_) | CreationContext::ManagedCanonical
    ) {
        super::creation_context::freeze(
            &mut planner_payload,
            &track,
            matches!(init, TrackInit::Fork { .. }),
            init_snapshot.as_ref(),
        )?;
    }
    if model.is_some() || reasoning_effort.is_some() {
        crate::planner_model::CardModelSelection::apply_to_payload(
            planner_payload
                .as_object_mut()
                .ok_or_else(|| CalmError::Internal("planner payload is not an object".into()))?,
            model.as_deref(),
            reasoning_effort.as_deref(),
        );
    }
    let mut structure = initialize_track_tx(
        tx,
        &track,
        (&planner_card_id_for_tx, &report_card_id_for_tx),
        TrackInitialization::Independent { planner_payload },
        write_for_tx.role_cache(),
        &acp_planner,
    )
    .await?;

    let mut init_projection = None;
    if let Some(InitialReportSnapshot {
        payload,
        mut doc,
        declarations,
        diagnostics,
        template_context: _,
    }) = init_snapshot
    {
        // The structural door takes no author, actor, event bus or CAS input, so this closure
        // can neither emit `track.report_edited` nor reach `guard_task_declarations`.
        let (persisted_report, projection) = crate::track_report::write::structural_init_report_tx(
            tx,
            crate::track_report::write::InitialReportTarget {
                report_card_id: structure.report.id.as_str(),
                track_id: track_id.as_str(),
                payload: &payload,
                doc: &mut doc,
                declarations: &declarations,
                diagnostics: &diagnostics,
            },
        )
        .await?;
        structure.report = persisted_report;
        init_projection = Some(projection);
    }

    if matches!(context, CreationContext::ManagedCanonical) {
        super::create::dormant::mint_dormant_carrier_tx(
            tx,
            &planner_card_id_for_tx,
            planner_provider,
        )
        .await?;
    }

    let mut events = structure.creation_events(&track, &actor_id_for_tx, creation_message);
    if let Some(projection) = init_projection {
        if !projection.changed_keys.is_empty() {
            events.push((
                actor_id_for_tx.clone(),
                EventScope::Track {
                    track: track_id.clone(),
                    area: area_id.clone(),
                },
                Event::PlanUpdated {
                    track_id,
                    changed_keys: projection.changed_keys,
                    agent_message: None,
                },
            ));
        }
        events.extend(projection.kernel_events);
    }
    Ok(((track, true), events))
}
/// Missing fork sources are invalid creation requests, including when the
/// source authorization read encounters the absence before report validation.
pub(super) fn fork_source_error(error: CalmError, source_track_id: &str) -> CalmError {
    if matches!(error, CalmError::NotFound(_)) {
        CalmError::BadRequest(format!(
            "track create: fork source track `{source_track_id}` does not exist"
        ))
    } else {
        error
    }
}

#[cfg(test)]
mod managed_tests {
    use super::*;
    use axum::extract::FromRef;

    #[tokio::test]
    async fn u9_canonical_historical_identity_replay_keeps_original_owner() {
        let (_tmp, repo, state) = crate::daily_planner::tests::fixture().await;
        let route = RouteState::from_ref(&state);
        let (_, area) = crate::routes::areas::ensure_system_area(&route)
            .await
            .unwrap();
        let request = NewTrack {
            area_id: area.id,
            title: "historical managed".into(),
            sort: None,
            cwd: String::new(),
            template_id: Some(crate::templates::DAILY_PLANNER.into()),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        };
        let identity = crate::managed_track::ManagedTrackIdentity {
            owner: "historical-managed".into(),
            identity: "original".into(),
            report_read_scope: crate::managed_track::ReportReadScope::Workspace,
            report_time_zone: crate::daily_planner::TIME_ZONE,
            tool_policy: crate::managed_track::ToolPolicy::Reports,
            kernel_controls_lifecycle: true,
        };
        // Exercise the original Historical structure owner to persist an actual
        // legacy allocation; the new entry must replay it without reminting.
        let (track, _, planner, _) = create_track_structure(
            route.clone(),
            ActorId::Kernel,
            request.clone(),
            CreateTrackOptions {
                human_source: None,
                admin: false,
                managed_identity: Some(identity.clone()),
                creator: None,
                creation_message: None,
                planner_provider: AgentProvider::Codex,
                model: None,
                reasoning_effort: None,
                folder_claim: FolderClaim::Skip,
                body_area_id: request.area_id.to_string(),
                normalized_cwd: String::new(),
                init: TrackInit::Template {
                    key: crate::templates::DAILY_PLANNER,
                    capabilities: &[],
                    binding: None,
                },
                workspace_plan: TrackWorkspacePlan::ManagedUnder(route.workspace_root.clone()),
                idempotency_claim: None,
            },
        )
        .await
        .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let replay = create_managed_track(route.clone(), request, identity)
            .await
            .unwrap();
        assert_eq!(replay.id, track.id);
        let pointers: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT c.session_id,t.root_session_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1")
            .bind(&planner).fetch_one(repo.pool()).await.unwrap();
        assert_eq!(pointers, (None, None));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM worker_sessions")
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            before
        );
    }
}
