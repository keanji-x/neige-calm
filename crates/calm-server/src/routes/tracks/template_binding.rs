//! Deferred binding shares admission, cumulative authority and report writes across REST and Planner tools.
use super::*;

pub(crate) struct PreparedBinding {
    admission: TemplateAdmission,
    report: InitialReportSnapshot,
    manifests: Vec<Manifest>,
    planner_card_id: Option<CardId>,
    plugin: std::sync::Arc<crate::plugin_host::PluginHost>,
}

pub(crate) async fn prepare(s: &RouteState, track: &Track, id: &str) -> Result<PreparedBinding> {
    let db = s
        .mcp_context
        .sqlite_db
        .as_ref()
        .ok_or_else(|| CalmError::Internal("template binding requires SQLite".into()))?;
    require_bindable(db.read_pool(), track).await?;
    let admission = admit_template(s, id).await.ok_or_else(|| {
        CalmError::BadRequest(format!("unknown or unavailable Track template `{id}`"))
    })?;
    if let Some(owner) =
        missing_required_template_owner(admission.key(), admission.binding.as_ref())
    {
        return Err(CalmError::BadRequest(format!(
            "template `{id}` requires `{}`; enable its built-in capability in Settings",
            owner.manifest().id
        )));
    }
    validate_input(&admission, track)?;
    let planner_card_id = s
        .repo
        .cards_by_track(track.id.as_str())
        .await?
        .into_iter()
        .find(|card| s.write.verify_role(&card.id) == Some(CardRole::Planner))
        .map(|card| card.id);
    let report = compile_template(admission.template)?;
    let (_, _, current) =
        crate::track_report::resolve_report_for_track(s.repo.as_ref(), track.id.as_str()).await?;
    let _ = crate::track_report::template_report_contract(&current, &report.payload)?;
    Ok(PreparedBinding {
        admission,
        report,
        planner_card_id,
        plugin: s.plugin.clone(),
        manifests: s.plugin.registry().list(),
    })
}

impl PreparedBinding {
    pub(crate) async fn apply_tx(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        track: &Track,
    ) -> Result<(Track, Vec<(ActorId, EventScope, Event)>)> {
        require_bindable(&mut **tx, track).await?;
        validate_input(&self.admission, track)?;
        let input = binding_input(&self.admission, track);
        // The registry retains stopped/disabled installed manifests. Uninstalled plugins have
        // no active authority to preserve; a later installation needs its own manifest-bound grant.
        if let Some(previous) = self
            .manifests
            .iter()
            .find(|m| track.plugin_scope.as_deref() == Some(m.id.as_str()))
            && self
                .admission
                .binding
                .as_ref()
                .is_none_or(|next| next.id != previous.id)
        {
            crate::track_plugin_access::grant_template_plugin_tx(tx, &track.id, previous).await?;
        }
        if let Some(owner) = self.admission.binding.as_ref() {
            crate::track_plugin_access::grant_template_plugin_tx(tx, &track.id, owner).await?;
        }
        calm_truth::db::sqlite::track_capabilities::stamp_tx(
            tx,
            &track.id,
            self.admission.template.capabilities(),
        )
        .await?;
        let updated = calm_truth::db::sqlite::track_bind_template_tx(
            tx,
            &track.id,
            self.admission.key(),
            self.admission.binding.as_ref().map(|m| m.id.as_str()),
            input,
        )
        .await?;
        let mut events = crate::track_report::write::apply_template_report_tx(
            tx,
            &updated,
            &self.report.payload,
        )
        .await?;
        if let Some(id) = self.planner_card_id {
            let mut card = calm_truth::db::sqlite::card_for_update_tx(tx, id.as_str()).await?;
            match &self.report.template_context {
                Some(context) => {
                    card.payload[crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY] =
                        serde_json::to_value(context)?;
                }
                None => {
                    card.payload
                        .as_object_mut()
                        .ok_or_else(|| {
                            CalmError::Internal("Planner payload must be an object".into())
                        })?
                        .remove(crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY);
                }
            }
            if crate::db::sqlite::conversation::card_has_canonical_storage(tx, id.as_str()).await? {
                // Runtime storage is worker_sessions on this upstream; last_thread_id
                // belongs to its persisted harness frame, not a card_runtime table.
                let started: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM conversation_input_submissions WHERE card_id=?1) OR EXISTS(SELECT 1 FROM worker_sessions WHERE card_id=?1 AND json_extract(handle_state_json,'$.last_thread_id') IS NOT NULL)",
                ).bind(id.as_str()).fetch_one(&mut **tx).await?;
                if started {
                    let mut text = String::from(
                        "Track configuration updated. Apply the selected working method and current plugin instructions to subsequent work.",
                    );
                    if let Some(context) = &self.report.template_context {
                        context.append_to(&mut text)?;
                    }
                    if let Some(input) = &updated.template_input {
                        crate::template_context::append_bound_input(&mut text, input);
                    }
                    crate::operation::planner_plugin_instructions::append_configuration_instructions_tx(
                        tx, &updated, &self.plugin, &mut text,
                    ).await?;
                    events.push((
                        ActorId::Kernel,
                        EventScope::Track {
                            track: updated.id.clone(),
                            area: updated.area_id.clone(),
                        },
                        Event::TrackWakeRequested {
                            track_id: updated.id.clone(),
                            source: "track_configuration".into(),
                            key: new_id(),
                            text,
                        },
                    ));
                } else {
                    creation_context::refresh(&mut card.payload, &updated)?;
                }
            }
            calm_truth::db::sqlite::planner_configuration_set_tx(tx, id.as_str(), &card.payload)
                .await?;
        }
        Ok((updated, events))
    }
}

async fn require_bindable<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    track: &Track,
) -> Result<()> {
    let managed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1)",
    )
    .bind(track.id.as_str())
    .fetch_one(executor)
    .await?;
    if managed {
        return Err(CalmError::Forbidden(
            "managed Track templates are kernel-maintained".into(),
        ));
    }
    Ok(())
}

/// Inputs belong to the bound plugin, not to cumulative grants. Preserve them only
/// across selections with the same owner; a new owner validates its own empty input.
fn binding_input<'a>(
    admission: &TemplateAdmission,
    track: &'a Track,
) -> Option<&'a serde_json::Value> {
    admission
        .binding
        .as_ref()
        .filter(|manifest| track.plugin_scope.as_deref() == Some(manifest.id.as_str()))
        .and(track.template_input.as_ref())
}

fn validate_input(admission: &TemplateAdmission, track: &Track) -> Result<()> {
    use crate::plugin_host::template_input::{TemplateInputOwner, validate_template_input_binding};
    let owner = match admission.binding.as_ref() {
        Some(manifest) => TemplateInputOwner::Plugin(manifest),
        None => TemplateInputOwner::NoBoundPlugin,
    };
    validate_template_input_binding(owner, binding_input(admission, track))?;
    Ok(())
}

/// Planner writes share the route admission and transaction owner, without restarting their caller.
pub(crate) async fn set_from_planner(
    state: &RouteState,
    identity: crate::mcp_server::registry::ToolCallIdentity,
    template_id: &str,
    message: Option<String>,
) -> Result<serde_json::Value> {
    let card = state
        .repo
        .card_get(&identity.card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("Planner card".into()))?;
    if state.write.verify_role(&card.id) != Some(CardRole::Planner) {
        return Err(CalmError::Forbidden(
            "only the Planner selects a working method".into(),
        ));
    }
    let _guard = crate::per_card_lock::lock_key(
        &state.track_delete_locks,
        &format!("template-binding:{}", card.track_id),
    )
    .await;
    let track = state
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound("Track".into()))?;
    let binding = prepare(state, &track, template_id).await?;
    let actor = identity.to_actor_id();
    let id = track.id;
    let plugin = state.plugin.clone();
    let ((updated, instructions), _) = write_with_actor_events_typed(
        state.repo.as_ref(),
        None,
        &state.events,
        &state.write,
        move |tx| {
            Box::pin(async move {
                let live = track_get_tx(tx, &id).await?;
                if identity.role != CardRole::Planner
                    || !identity.session_is_active(tx, live.id.as_str()).await?
                {
                    return Err(CalmError::Forbidden(
                        "Planner session is no longer active".into(),
                    ));
                }
                if live.closed_at.is_some() {
                    return Err(CalmError::Conflict("Track is closed".into()));
                }
                let (updated, mut events) = binding.apply_tx(tx, &live).await?;
                events.retain(|(_, _, event)| !matches!(event, Event::TrackWakeRequested { .. }));
                for (event_actor, _, event) in &mut events {
                    if matches!(
                        event,
                        Event::CardUpdated(_)
                            | Event::TrackReportEdited { .. }
                            | Event::PlanUpdated { .. }
                    ) {
                        *event_actor = actor.clone();
                    }
                }
                let card = calm_truth::db::sqlite::card_for_update_tx(tx, card.id.as_str()).await?;
                let mut instructions = String::new();
                if let Some(context) =
                    crate::template_context::TemplateContext::from_card_payload(&card.payload)?
                {
                    context.append_to(&mut instructions)?;
                }
                if let Some(input) = &updated.template_input {
                    crate::template_context::append_bound_input(&mut instructions, input);
                }
                crate::operation::planner_plugin_instructions::append_configuration_instructions_tx(
                    tx, &updated, &plugin, &mut instructions,
                ).await?;
                events.push((
                    actor,
                    EventScope::Track {
                        track: updated.id.clone(),
                        area: updated.area_id.clone(),
                    },
                    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        updated.clone(),
                        message,
                    )),
                ));
                Ok(((updated, instructions), events))
            })
        },
    )
    .await?;
    Ok(
        serde_json::json!({"ok": true, "template_id": updated.template_id,
        "plugin_scope": updated.plugin_scope, "instructions": instructions}),
    )
}
