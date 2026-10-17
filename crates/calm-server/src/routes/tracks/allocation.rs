//! The one protected Track structure allocation step. Callers own source
//! admission and whether postcommit materialization/start is requested.
use super::*;
use crate::state::WriteContext;
use sqlx::{Sqlite, Transaction};
use std::sync::{Arc, OnceLock};

pub(super) struct TrackAllocationOwners {
    pub write: WriteContext,
    pub templates: &'static TemplateRoster,
}

pub(super) type TrackAllocationWrite = ((Track, bool), Vec<(ActorId, EventScope, Event)>);

// This is the existing legacy DB chain, shared unchanged by both compositions.
// Its transaction helpers still require the raw caches rather than verification.
#[allow(deprecated)]
pub(super) async fn allocate_track_structure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    actor_id_for_tx: ActorId,
    p: NewTrack,
    options: CreateTrackOptions,
    ids: (String, String),
    owners: TrackAllocationOwners,
    replay_tx: Arc<OnceLock<String>>,
) -> Result<TrackAllocationWrite> {
    let CreateTrackOptions {
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
    let (planner_card_id_for_tx, report_card_id_for_tx) = ids;
    let TrackAllocationOwners {
        write: write_for_tx,
        templates,
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
    // Claim scan + insert, atomic with the track row; must stay first so every branch
    // either rolls back or leaves the claim table consistent.
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
            let source_track = track_get_tx(tx, &source_id).await.map_err(|error| {
                if matches!(error, CalmError::NotFound(_)) {
                    CalmError::BadRequest(format!(
                        "track create: fork source track `{source_track_id}` does not exist"
                    ))
                } else {
                    error
                }
            })?;
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

    let mut planner_payload = planner_card_payload(None, planner_provider, init_snapshot.as_ref())?;
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
