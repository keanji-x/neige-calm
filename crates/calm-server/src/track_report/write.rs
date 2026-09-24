//! The report write boundary. [`persist`] is the one function that performs a report edit and
//! is private to this module; every caller outside goes through a purpose-specific entry point.
//! [`persist_report`] is the test-only escape hatch (`cfg(any(test, feature = "fixtures"))`).

use super::*;

/// The arguments of [`structural_init_report_tx`], as one value. The `doc` arrives already
/// authoritative and this door applies no op to it: routing through `apply_report_op` would
/// advance `doc_rev` and re-stamp every block's `rev`, breaking the on-the-wire fork contract.
pub(crate) struct InitialReportTarget<'a> {
    /// INSERTed earlier in the same transaction, which is why there is no CAS input here.
    pub report_card_id: &'a str,
    /// The track that owns the card, used for the task projection.
    pub track_id: &'a str,
    /// The projected payload — the JSON read cache mirroring `doc`.
    pub payload: &'a TrackReportPayload,
    /// The authoritative CRDT document. `&mut` for serialization only.
    pub doc: &'a mut ReportDoc,
    /// The task declarations projected out of the report's blocks.
    pub declarations: &'a [calm_types::report_blocks::tasks::TaskDeclaration],
    /// Per-block local diagnostics, aligned with the blocks the declarations came from.
    pub diagnostics: &'a [Vec<calm_types::report_blocks::tasks::Diagnostic>],
}

/// Pins each spelling `use super::*` brings in to its canonical path, so a same-named shadow
/// type defined in this file is a compile error; `fork_guard_exemption_invariant.rs` requires these lines.
const _: fn() = || {
    fn identical<T>(value: T) -> T {
        value
    }

    let _: fn(
        ::sqlx::Transaction<'static, ::sqlx::Sqlite>,
    ) -> sqlx::Transaction<'static, sqlx::Sqlite> = identical;
    let _: fn(::core::result::Result<u8, u8>) -> Result<u8, u8> = identical;
    let _: fn(crate::model::Card) -> Card = identical;
    let _: fn(crate::db::sqlite::TaskProjectionOutcome) -> TaskProjectionOutcome = identical;
    let _: fn(crate::error::CalmError) -> CalmError = identical;
    let _: fn(&'static ::core::primitive::str) -> &'static str = identical;
    let _: fn(::calm_types::track_report::TrackReportPayload) -> TrackReportPayload = identical;
    let _: fn(crate::track_report_doc::ReportDoc) -> ReportDoc = identical;
    let _: fn(::std::vec::Vec<u8>) -> Vec<u8> = identical;
    let _: fn(
        ::calm_types::report_blocks::tasks::TaskDeclaration,
    ) -> calm_types::report_blocks::tasks::TaskDeclaration = identical;
    let _: fn(
        ::calm_types::report_blocks::tasks::Diagnostic,
    ) -> calm_types::report_blocks::tasks::Diagnostic = identical;
};

/// `POST /api/tracks/{id}/report` — the user's wholesale report replace. Attribution is not a
/// parameter: only a User edit can be recorded, by construction.
pub(crate) async fn rest_user_replace(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    target: ReportEditTarget,
    next: TrackReportPayload,
    if_doc_rev: u64,
) -> Result<Card, CalmError> {
    let ((updated, _block), _) = persist(
        repo,
        events,
        write,
        ActorId::User,
        EditAuthor::User,
        target,
        PersistPurpose::Edit(ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        }),
        None,
        None,
        false,
        None,
    )
    .await?;
    Ok(updated)
}

/// `POST|PATCH|DELETE /api/tracks/{id}/report/blocks*` — the user's typed block-channel edits.
/// `auto_promote_draft = false` is fixed: a Draft track with a report is a legal state.
pub(crate) async fn rest_user_block_op(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    target: ReportEditTarget,
    op: ReportDocOp,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    persist(
        repo,
        events,
        write,
        ActorId::User,
        EditAuthor::User,
        target,
        PersistPurpose::Edit(op),
        None,
        None,
        false,
        None,
    )
    .await
    .map(|((card, trace), _)| (card, trace.block))
}

/// The explicit User start purpose. Attribution, lifecycle intent, and task
/// shape are fixed here; ordinary REST block edits remain ordinary edits.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn rest_user_start(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    target: ReportEditTarget,
    key: String,
    goal: String,
    if_doc_rev: u64,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    persist(
        repo,
        events,
        write,
        ActorId::User,
        EditAuthor::User,
        target,
        PersistPurpose::UserStart {
            key,
            goal,
            if_doc_rev,
        },
        None,
        None,
        false,
        None,
    )
    .await
    .map(|((card, trace), _)| (card, trace.block))
}

#[derive(Clone)]
enum PersistPurpose {
    Edit(ReportDocOp),
    Dispatch {
        identity: crate::mcp_server::registry::ToolCallIdentity,
        args: super::dispatch::DispatchArgs,
        plugin_tools: super::dispatch::PluginToolAdmission,
        task_budget_default: i64,
    },
    Repair {
        identity: crate::mcp_server::registry::ToolCallIdentity,
        args: crate::file_delivery::repair::RepairArgs,
        task_budget_default: i64,
    },
    Replace {
        identity: crate::mcp_server::registry::ToolCallIdentity,
        args: crate::task_replace::ReplaceArgs,
    },
    UserStart {
        key: String,
        goal: String,
        if_doc_rev: u64,
    },
}

/// The agent-MCP funnel. Takes its attribution, auto-promote verdict and recorder-shadow probe
/// from the caller and validates none of them: the boundary bounds the set of doors, not what
/// a caller says when it walks through this one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_report_op(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    target: ReportEditTarget,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Arc<dyn RecorderShadowProbe>,
) -> Result<(Card, ReportOpTrace), CalmError> {
    persist(
        repo,
        events,
        write,
        actor,
        author,
        target,
        PersistPurpose::Edit(op),
        agent_message,
        lifecycle,
        auto_promote_draft,
        Some(recorder_shadow),
    )
    .await
    .map(|(edit, _)| edit)
}

/// Planner-only semantic dispatch; authorization and replay stay inside persist.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn planner_dispatch(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    identity: crate::mcp_server::registry::ToolCallIdentity,
    target: ReportEditTarget,
    args: super::dispatch::DispatchArgs,
    plugin_tools: super::dispatch::PluginToolAdmission,
    task_budget_default: i64,
    recorder_shadow: Arc<dyn RecorderShadowProbe>,
) -> Result<serde_json::Value, CalmError> {
    let (_, response) = persist(
        repo,
        events,
        write,
        identity.to_actor_id(),
        EditAuthor::Planner,
        target,
        PersistPurpose::Dispatch {
            identity,
            args: args.normalize()?,
            plugin_tools,
            task_budget_default,
        },
        None,
        None,
        false,
        Some(recorder_shadow),
    )
    .await?;
    response.ok_or_else(|| CalmError::Internal("dispatch snapshot missing".into()))
}

/// Planner-only single-round candidate repair; authorization and replay stay inside persist.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn planner_repair(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    identity: crate::mcp_server::registry::ToolCallIdentity,
    target: ReportEditTarget,
    args: crate::file_delivery::repair::RepairArgs,
    task_budget_default: i64,
    recorder_shadow: Arc<dyn RecorderShadowProbe>,
) -> Result<serde_json::Value, CalmError> {
    let (_, response) = persist(
        repo,
        events,
        write,
        identity.to_actor_id(),
        EditAuthor::Planner,
        target,
        PersistPurpose::Repair {
            identity,
            args,
            task_budget_default,
        },
        None,
        None,
        false,
        Some(recorder_shadow),
    )
    .await?;
    response.ok_or_else(|| CalmError::Internal("repair snapshot missing".into()))
}

/// Planner-only task replacement (#1785): stop, append the successor, write the receipt — one
/// transaction; authorization and replay stay inside persist.
pub(crate) async fn planner_replace(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    identity: crate::mcp_server::registry::ToolCallIdentity,
    target: ReportEditTarget,
    args: crate::task_replace::ReplaceArgs,
    recorder_shadow: Arc<dyn RecorderShadowProbe>,
) -> Result<serde_json::Value, CalmError> {
    let (_, response) = persist(
        repo,
        events,
        write,
        identity.to_actor_id(),
        EditAuthor::Planner,
        target,
        PersistPurpose::Replace { identity, args },
        None,
        None,
        false,
        Some(recorder_shadow),
    )
    .await?;
    response.ok_or_else(|| CalmError::Internal("replace snapshot missing".into()))
}

/// The structural door: track creation laying a forked or templated report onto the report card
/// it just INSERTed. Not an edit and never calls [`persist`]; shares only the row write and the
/// task projection. Takes no author, actor, revision or policy on purpose — the signature is
/// pinned by `tests/cases/fork_guard_exemption_invariant.rs`.
pub(crate) async fn structural_init_report_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: InitialReportTarget<'_>,
) -> Result<(Card, TaskProjectionOutcome), CalmError> {
    let (card, projection) = write_report_row_and_project_tx(
        tx,
        target.report_card_id,
        target.track_id,
        target.payload,
        target.doc,
        target.declarations,
        target.diagnostics,
    )
    .await?;
    // Fail closed: this door has no actor to attribute kernel events to and no bus to put them on.
    // Empty on every production path today; a future create-time projection event must not escape
    // into the create closure's event list unnoticed.
    if !projection.kernel_events.is_empty() {
        return Err(CalmError::Internal(format!(
            "structural_init_report_tx: task projection produced {} kernel event(s) while \
             creating track {}; the structural door has no actor to attribute them to and no \
             bus to put them on (#1252 Q12). If create-time projection events are now a real \
             case, decide explicitly who emits them and where.",
            projection.kernel_events.len(),
            target.track_id
        )));
    }
    Ok((card, projection))
}

/// Test-only direct access to the boundary: tests write as any `EditAuthor`, which production
/// code must not. `fixtures` is an ordinary additive feature; the guarantee is a convention in `Cargo.toml`.
#[cfg(any(test, feature = "fixtures"))]
#[allow(clippy::too_many_arguments)]
pub async fn persist_report(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
    next: TrackReportPayload,
    if_doc_rev: u64,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
) -> Result<Card, CalmError> {
    let ((updated, _block), _) = persist(
        repo,
        events,
        write,
        actor,
        author,
        ReportEditTarget {
            track,
            report_card,
            current_payload,
        },
        PersistPurpose::Edit(ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        }),
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
    )
    .await?;
    Ok(updated)
}

/// Apply one [`ReportDocOp`] to the report card's CRDT, write the row, and emit
/// `Event::CardUpdated` + `Event::TrackReportEdited` from the same transaction. Private: every
/// report edit goes through here. `target.current_payload` seeds only the first-time
/// `from_payload` branch; once `body_crdt` is non-NULL the doc is the source.
#[allow(clippy::too_many_arguments)]
async fn persist(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    target: ReportEditTarget,
    purpose: PersistPurpose,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
) -> Result<((Card, ReportOpTrace), Option<serde_json::Value>), CalmError> {
    // Zero-event replay: authorize/read in the same transaction, then roll it back.
    const DISPATCH_REPLAY: &str = "planner dispatch receipt replay";
    let replay = Arc::new(std::sync::Mutex::new(None));
    let replay_out = replay.clone();
    let ReportEditTarget {
        track,
        report_card,
        current_payload,
    } = target;
    let report_card_id = report_card.id.clone();
    let track_id = track.id.clone();
    let area_id = track.area_id.clone();
    let scope = EventScope::Card {
        card: report_card_id.clone(),
        track: track_id.clone(),
        area: area_id.clone(),
    };
    let track_scope = EventScope::Track {
        track: track_id.clone(),
        area: area_id,
    };
    let report_card_id_inner = report_card_id.clone();
    let track_id_for_event = track_id.clone();
    let result = write_with_actor_events_typed::<_, _>(
        repo,
        None,
        events,
        write,
        move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let track_id = track_id_for_event.clone();
            let scope = scope.clone();
            let track_scope = track_scope.clone();
            let current_payload = current_payload.clone();
            let purpose = purpose.clone();
            let actor = actor.clone();
            let agent_message = agent_message.clone();
            let recorder_shadow = recorder_shadow.clone();
            Box::pin(async move {
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if let PersistPurpose::Dispatch { identity, .. }
                    | PersistPurpose::Repair { identity, .. }
                    | PersistPurpose::Replace { identity, .. } = &purpose
                {
                    super::dispatch::authorize_tx(tx, identity, &track_id, &id).await?;
                }
                if auto_promote_draft
                    && let Some(auto_events) = auto_promote_draft_in_tx(tx, &track_id).await?
                {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, track_scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &track_id,
                        target,
                        &actor,
                        agent_message.clone().unwrap_or_default(),
                    )
                    .await?
                {
                    if let Some(probe) = recorder_shadow.as_ref() {
                        probe
                            .record(tx, RecorderShadowDecisionKind::TrackLifecycle)
                            .await?;
                    }
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), track_scope.clone(), event)),
                    );
                }
                if let Some(probe) = recorder_shadow.as_ref() {
                    probe
                        .record(tx, RecorderShadowDecisionKind::ReportWrite)
                        .await?;
                }
                if let PersistPurpose::Dispatch { args, task_budget_default, .. } = &purpose
                    && let Some(receipt) = super::dispatch::lookup_tx(tx, &track_id, args).await?
                {
                    let response = super::dispatch::snapshot_tx(tx, &track_id, &receipt, args, *task_budget_default).await?;
                    let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
                        "SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at FROM cards WHERE id=?1"
                    ).bind(&id).fetch_one(&mut **tx).await?;
                    *replay_out.lock().map_err(|_| CalmError::Internal("dispatch replay lock poisoned".into()))? = Some((Card::from(card), response));
                    return Err(CalmError::Conflict(DISPATCH_REPLAY.into()));
                }
                if let PersistPurpose::Repair { args, task_budget_default, .. } = &purpose {
                    args.validate()?;
                    if let Some(receipt) = crate::file_delivery::repair::lookup_tx(tx, track_id.as_str(), &args.producer).await? {
                        if receipt.args != *args { return Err(CalmError::Conflict("repair source already has a different reason".into())); }
                        crate::file_delivery::repair::validate_lineage_tx(tx, &receipt).await?;
                        let response = super::repair::snapshot_tx(tx, &receipt, *task_budget_default).await?;
                        let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
                            "SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at FROM cards WHERE id=?1"
                        ).bind(&id).fetch_one(&mut **tx).await?;
                        *replay_out.lock().map_err(|_|CalmError::Internal("repair replay lock poisoned".into()))? = Some((Card::from(card), response));
                        return Err(CalmError::Conflict(DISPATCH_REPLAY.into()));
                    }
                }
                if let PersistPurpose::Replace { args, .. } = &purpose
                    && let Some(response) = super::replace::replay_tx(tx, track_id.as_str(), args).await?
                {
                    let card = super::replace::report_card_tx(tx, &id).await?;
                    *replay_out.lock().map_err(|_| CalmError::Internal("replace replay lock poisoned".into()))? = Some((card, response));
                    return Err(CalmError::Conflict(DISPATCH_REPLAY.into()));
                }
                if let PersistPurpose::Dispatch { args, plugin_tools, .. } = &purpose
                    && let Some(refusal) = plugin_tools.refusal(args.plugin_tools()) {
                    return Err(CalmError::Forbidden(refusal));
                }
                // A new Planner declaration preserves the existing Draft promotion.
                // Receipt replay returned above and cannot promote or resume work.
                if let PersistPurpose::Dispatch { .. } = &purpose
                    && let Some(auto_events) = auto_promote_draft_in_tx(tx, &track_id).await?
                {
                    events.extend(auto_events.into_iter().map(|event|
                        (ActorId::Kernel, track_scope.clone(), event)));
                }
                // 1. Load (or lazy-init) the CRDT doc. Loaded docs may still carry the old layout (no block
                //    map) — migrate in place using the payload's block ids as hint; written back in this tx.
                let existing = card_body_crdt_get_tx(tx, &id).await?;
                let mut doc = match existing {
                    Some(bytes) => {
                        let mut doc = ReportDoc::from_bytes(&bytes).map_err(|e| {
                            CalmError::Internal(format!(
                                "track_report: load CRDT for card {id}: {e}"
                            ))
                        })?;
                        doc.ensure_blocks_layout(current_payload.blocks.as_deref())
                            .map_err(|e| {
                                CalmError::Internal(format!(
                                    "track_report: migrate CRDT block layout for card {id}: {e}"
                                ))
                            })?;
                        doc
                    }
                    // Safe: current_payload was read outside the tx, but is only consulted while body_crdt is
                    // still NULL in-tx; SQLite's single writer means nothing populated the blob in between.
                    None => {
                        let mut doc = ReportDoc::from_payload(&current_payload);
                        doc.ensure_blocks_layout(current_payload.blocks.as_deref())
                            .map_err(|e| {
                                CalmError::Internal(format!(
                                    "track_report: migrate seeded CRDT block layout for card {id}: {e}"
                                ))
                            })?;
                        doc
                    }
                };
                // 2. Capture the pre-write projection for the edit-log entry.
                let (summary_before, body_before) = doc.project().map_err(|e| {
                    CalmError::Internal(format!("track_report: project CRDT for card {id}: {e}"))
                })?;
                let dispatch_key = format!("dispatch-{}", uuid::Uuid::new_v4().simple());
                let mut repair_receipt = None;
                let mut replace_staged = None;
                let op = match purpose.clone() {
                    PersistPurpose::Repair { args, .. } => {
                        let mut receipt = crate::file_delivery::repair::prepare_tx(tx, track_id.as_str(), &id, &args).await?;
                        let first = super::repair::prepare(&doc, &receipt.repair)?;
                        let (created, _) = apply_persisted_report_op(&mut doc, &first, author)?;
                        receipt.repair.block_id = created.block.ok_or_else(||CalmError::Internal("repair block outcome missing".into()))?.id;
                        let second = super::repair::prepare(&doc, &receipt.reviewer)?;
                        repair_receipt = Some(receipt);
                        second
                    }
                    PersistPurpose::Replace { args, .. } => {
                        let (op, staged) = super::replace::stage_tx(tx, track_id.as_str(), &doc, &args).await?;
                        replace_staged = Some(staged);
                        op
                    }
                    PersistPurpose::Edit(op) => op,
                    PersistPurpose::Dispatch { args, .. } => {
                        super::dispatch::prepare(&doc, &args, &dispatch_key)?
                    }
                    PersistPurpose::UserStart { key, goal, if_doc_rev } => {
                        let op = super::user_start::prepare_tx(
                            tx, &track_id, &doc, &key, &goal, if_doc_rev,
                        ).await?;
                        let current = track_get_tx(tx, &track_id).await?;
                        if current.lifecycle == TrackLifecycle::Draft
                            && let Some(transitions) = apply_requested_transition_in_tx(
                                tx, &track_id, TrackLifecycle::Planning, &ActorId::User,
                                "Start independent task".to_string(),
                            ).await?
                        {
                            events.extend(transitions.into_iter().map(|event|
                                (ActorId::User, track_scope.clone(), event)));
                        }
                        op
                    }
                };
                // 3. Apply the op; `if_rev` checks happen in here against the CRDT truth, a conflict aborts the tx.
                let (trace, doc_rev) = apply_persisted_report_op(&mut doc, &op, author)?;
                let outcome = &trace.block;
                // 4. Project back — the CRDT block map is the source of truth; nothing is re-derived at the JSON layer.
                let (summary_after, body_after) = doc.project().map_err(|e| {
                    CalmError::Internal(format!(
                        "track_report: project CRDT post-op for card {id}: {e}"
                    ))
                })?;
                let mut projected_payload =
                    TrackReportPayload::new(summary_after.clone(), body_after.clone());
                projected_payload.doc_rev = doc_rev;
                projected_payload.blocks = Some(doc.blocks_snapshot().map_err(|e| {
                    CalmError::Internal(format!(
                        "track_report: snapshot CRDT blocks for card {id}: {e}"
                    ))
                })?);
                let blocks = projected_payload.blocks.as_deref().unwrap_or_default();
                let (declarations, block_diagnostics) =
                    calm_types::report_blocks::tasks::project_task_declarations(blocks);
                // 5. The row write and the task projection, in the one order both writers use.
                let (updated, mut task_projection) = write_report_row_and_project_tx(
                    tx,
                    &id,
                    track_id.as_str(),
                    &projected_payload,
                    &mut doc,
                    &declarations,
                    &block_diagnostics,
                )
                .await?;
                let dispatch_response = if let PersistPurpose::Dispatch { args, task_budget_default, .. } = &purpose {
                    let block = outcome.as_ref().ok_or_else(|| CalmError::Internal("dispatch block outcome missing".into()))?;
                    let receipt = super::dispatch::DispatchReceipt {
                        name: args.name().to_owned(), task_key: dispatch_key,
                        report_card_id: id.clone(), block_id: block.id.clone(),
                        created_at_ms: crate::model::now_ms(),
                    };
                    super::dispatch::insert_tx(tx, &track_id, args, &receipt).await?;
                    Some(super::dispatch::snapshot_tx(tx, &track_id, &receipt, args, *task_budget_default).await?)
                } else if let PersistPurpose::Repair { task_budget_default, .. } = &purpose {
                    let receipt = repair_receipt.as_mut().ok_or_else(||CalmError::Internal("repair receipt missing".into()))?;
                    receipt.reviewer.block_id = outcome.as_ref().ok_or_else(||CalmError::Internal("repair review block outcome missing".into()))?.id.clone();
                    crate::file_delivery::repair::insert_tx(tx, receipt).await?;
                    Some(super::repair::snapshot_tx(tx, receipt, *task_budget_default).await?)
                } else if let Some(staged) = replace_staged {
                    staged.add_stopped_key(&mut task_projection.changed_keys);
                    Some(super::replace::finish_tx(tx, track_id.as_str(), staged).await?)
                } else { None };
                //    Then two events on the same card scope: `CardUpdated` first, so a subscriber sees the
                //    generic "row changed" signal before the structured edit-log entry.
                let report_edited = Event::TrackReportEdited {
                    track_id: track_id.clone(),
                    card_id: report_card_id,
                    author,
                    // Kept on the event wire for compatibility; nothing writes it.
                    author_plugin_id: None,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before,
                    summary_after,
                    body_before,
                    body_after,
                    agent_message,
                };
                events.push((
                    actor.clone(),
                    scope.clone(),
                    Event::CardUpdated(updated.clone()),
                ));
                events.push((actor.clone(), scope, report_edited));
                if !task_projection.changed_keys.is_empty() {
                    events.push((
                        actor.clone(),
                        track_scope,
                        Event::PlanUpdated {
                            track_id,
                            changed_keys: task_projection.changed_keys,
                            agent_message: None,
                        },
                    ));
                }
                events.extend(task_projection.kernel_events);
                Ok((((updated, trace), dispatch_response), events))
            })
        },
    )
    .await;
    match result {
        Ok((updated, _ids)) => Ok(updated),
        Err(CalmError::Conflict(message)) if message == DISPATCH_REPLAY => {
            let (card, response) = replay
                .lock()
                .map_err(|_| CalmError::Internal("dispatch replay lock poisoned".into()))?
                .take()
                .ok_or_else(|| CalmError::Internal("dispatch replay lost its receipt".into()))?;
            Ok(((card, ReportOpTrace::default()), Some(response)))
        }
        Err(error) => Err(error),
    }
}

/// Write the report card's row (JSON cache + CRDT bytes) and then reproject the track's tasks.
/// The order is the contract: the block-existence leg of task projection resolves `refs` by
/// reading the report card's payload cache, so the cache must already hold this write's
/// snapshot — swapped, every ref into new content silently collects `reference_missing`.
async fn write_report_row_and_project_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    report_card_id: &str,
    track_id: &str,
    payload: &TrackReportPayload,
    doc: &mut ReportDoc,
    declarations: &[calm_types::report_blocks::tasks::TaskDeclaration],
    diagnostics: &[Vec<calm_types::report_blocks::tasks::Diagnostic>],
) -> Result<(Card, TaskProjectionOutcome), CalmError> {
    // The one-shot contract-header check on the flat projection of every write through both doors,
    // before the row write: a rejection aborts the persist and no event is emitted.
    match calm_types::report_contract::check_document(&payload.body) {
        Ok(_) => {}
        Err(error @ HeaderError::Internal(_)) => {
            // A non-canonical header here means an ingress skipped `normalize_header` — a kernel bug, not
            // the caller's. A row stored non-canonical before the ingress existed hits this 500 on any
            // edit that leaves line 1 untouched.
            return Err(CalmError::Internal(format!(
                "track_report: report contract header: {error}"
            )));
        }
        Err(error) => {
            return Err(CalmError::BadRequest(format!(
                "report contract header: {error}"
            )));
        }
    }
    let payload_value = serde_json::to_value(payload).map_err(|e| {
        CalmError::Internal(format!("track_report: serialize projected payload: {e}"))
    })?;
    let patch = CardPatch {
        title: None,
        kind: None,
        sort: None,
        payload: Some(payload_value),
        deletable: None,
    };
    let updated = card_update_with_crdt_tx(tx, report_card_id, patch, doc.to_bytes()).await?;
    let projection = project_tasks_tx(tx, track_id, declarations, diagnostics).await?;
    Ok((updated, projection))
}
