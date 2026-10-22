//! Historical receipt planning and creation for excluded kinds.
use super::*;

/// Parse and validate the first-message half of the request, and pick the arm — before
/// `create_track` validates, let alone mints, anything.
pub(in crate::routes::tracks) async fn plan_first_message(
    s: &RouteState,
    headers: &HeaderMap,
    first_message: Option<String>,
    area_id: &str,
    shape: CreateRequestShape,
) -> Result<CreateShape<KeyedPlan, KeyedPlan<Option<MessageLessPlan>, MessageLessResume>>> {
    // The header is parsed BEFORE the `first_message` fork because it decides the
    // message-less fork too; a malformed key on a message-less create is a 400.
    let idempotency_key = track_create_key(headers)?;
    let Some(text) = first_message else {
        // No key means the legacy path verbatim; a key opts this shape into the same binding
        // row the `first_message` path uses.
        let Some(idempotency_key) = idempotency_key else {
            return Ok(CreateShape::MessageLess(KeyedPlan::Mint(None)));
        };
        return Ok(CreateShape::MessageLess(
            plan_message_less(s, area_id, shape, idempotency_key).await?,
        ));
    };
    let idempotency_key = idempotency_key.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required when `first_message` is present, so a retried create cannot mint a second track or deliver the message twice"
                .into(),
        )
    })?;
    let create_request_sha256 = create_request_digest(&shape)?;
    Ok(CreateShape::WithFirstMessage(
        plan_keyed_create(
            s,
            KeyedCreate {
                area_id: area_id.to_string(),
                idempotency_key,
                create_request_sha256,
                text,
            },
        )
        .await?,
    ))
}

/// The keyed create's decision, before anything is validated or minted: the binding lookup,
/// the fingerprint check and the arm. `POST /api/tracks` and `neige_track_add` both run it.
pub(in crate::routes::tracks) async fn plan_keyed_create(
    s: &RouteState,
    request: KeyedCreate,
) -> Result<KeyedPlan> {
    let KeyedCreate {
        area_id,
        idempotency_key,
        create_request_sha256,
        text,
    } = &request;
    let area_id = area_id.as_str();
    // Run before the folder claim, the track row, the cards and `materialize_workspace`,
    // so a rejected message leaves no track behind.
    validate_first_message(text)?;

    let base_key = derive_track_create_operation_key(area_id, idempotency_key);
    // Taken before either lookup, released when the plan is dropped at the end of the request.
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;

    // Lookup 1 — a row that committed with the id, so it is still answered after every
    // failure that leaves no operation row behind.
    let binding = s
        .repo
        .track_create_idempotency_get(area_id, idempotency_key)
        .await?;
    if let Some(binding) = binding.as_ref() {
        crate::conversation_inputs::require_historical_storage(
            s.repo.as_ref(),
            &binding.planner_card_id,
        )
        .await?;
        ensure_binding_create_matches(
            binding,
            create_request_sha256,
            idempotency_key,
            CreateShape::WithFirstMessage(()),
        )?;
    }

    let (plan, chosen_existing) = first_message_attempt(s, request, same_key_claim).await?;
    let operation_key = &plan.operation_key;

    match (binding, chosen_existing) {
        (None, None) => Ok(KeyedPlan::Mint(plan)),
        (None, Some(_)) => Err(CalmError::Internal(format!(
            "operation {operation_key} exists under this Idempotency-Key but no \
             track_create_idempotency row does. The binding commits inside the transaction that \
             mints the track, strictly before the operation is submitted, so this state is not \
             reachable from POST /api/tracks. Refusing rather than minting: a mint here would \
             commit a track and then collide on the operation's unique key, leaving an orphan \
             track behind a 409."
        ))),
        (Some(binding), chosen) => Ok(KeyedPlan::Resume(bound_resume(
            binding, plan, chosen, &base_key,
        )?)),
    }
}

/// The message-less twin of [`plan_first_message`]'s keyed half: one lookup, a two-cell
/// table. The lock is taken on the SAME derived key, so the two shapes serialize
/// against each other on the one binding row.
async fn plan_message_less(
    s: &RouteState,
    area_id: &str,
    shape: CreateRequestShape,
    idempotency_key: String,
) -> Result<KeyedPlan<Option<MessageLessPlan>, MessageLessResume>> {
    let create_request_sha256 = create_request_digest(&shape)?;
    let base_key = derive_track_create_operation_key(area_id, &idempotency_key);
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;
    let binding = s
        .repo
        .track_create_idempotency_get(area_id, &idempotency_key)
        .await?;
    let Some(binding) = binding else {
        return Ok(KeyedPlan::Mint(Some(MessageLessPlan {
            idempotency_key,
            create_request_sha256,
            _same_key_claim: same_key_claim,
        })));
    };
    crate::conversation_inputs::require_historical_storage(
        s.repo.as_ref(),
        &binding.planner_card_id,
    )
    .await?;
    // The create shape is permanent once its track commits, so a mismatching request is a
    // conflict rather than something to act on.
    ensure_binding_create_matches(
        &binding,
        &create_request_sha256,
        &idempotency_key,
        CreateShape::MessageLess(()),
    )?;
    Ok(KeyedPlan::Resume(MessageLessResume {
        track_id: binding.track_id,
        planner_card_id: binding.planner_card_id,
        report_card_id: binding.report_card_id,
        _same_key_claim: same_key_claim,
    }))
}

/// A receipt always resumes its bound identity. There is no binding-miss/mint arm.
pub(in crate::routes::tracks) async fn plan_historical_receipt(
    s: &RouteState,
    headers: &HeaderMap,
    request: &super::super::CreateTrackRequest,
    binding: crate::db::sqlite::TrackCreateBinding,
) -> Result<CreateShape<ResumeFirstMessage, MessageLessResume>> {
    let key = track_create_key(headers)?.ok_or_else(|| {
        CalmError::BadRequest("a creation receipt requires its Idempotency-Key".into())
    })?;
    let sha = create_request_digest(&CreateRequestShape::from_request(request)?)?;
    let base_key = derive_track_create_operation_key(request.area_id.as_str(), &key);
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;
    crate::conversation_inputs::require_historical_storage(
        s.repo.as_ref(),
        &binding.planner_card_id,
    )
    .await?;
    match &request.first_message {
        Some(text) => {
            validate_first_message(text)?;
            ensure_binding_create_matches(&binding, &sha, &key, CreateShape::WithFirstMessage(()))?;
            let (plan, chosen) = first_message_attempt(
                s,
                KeyedCreate {
                    area_id: request.area_id.to_string(),
                    idempotency_key: key,
                    create_request_sha256: sha,
                    text: text.clone(),
                },
                same_key_claim,
            )
            .await?;
            Ok(CreateShape::WithFirstMessage(bound_resume(
                binding, plan, chosen, &base_key,
            )?))
        }
        None => {
            ensure_binding_create_matches(&binding, &sha, &key, CreateShape::MessageLess(()))?;
            Ok(CreateShape::MessageLess(MessageLessResume {
                track_id: binding.track_id,
                planner_card_id: binding.planner_card_id,
                report_card_id: binding.report_card_id,
                _same_key_claim: same_key_claim,
            }))
        }
    }
}

fn bound_resume(
    binding: crate::db::sqlite::TrackCreateBinding,
    plan: FirstMessagePlan,
    chosen: Option<crate::operation::Operation>,
    base_key: &str,
) -> Result<ResumeFirstMessage> {
    let (arm, cwd) = match chosen {
        Some(op) => {
            let payload: PlannerHarnessStartOperationPayload = serde_json::from_value(op.payload)?;
            ensure_replay_message_matches(&payload, &plan)?;
            (PriorArm::Replay, Some(payload.cwd))
        }
        None => {
            ensure_binding_message_matches(&binding, &plan, plan.operation_key != base_key)?;
            (PriorArm::GenuineRetry, None)
        }
    };
    Ok(ResumeFirstMessage {
        plan,
        prior: PriorAttempt {
            arm,
            track_id: binding.track_id,
            planner_card_id: binding.planner_card_id,
            report_card_id: binding.report_card_id,
            cwd,
        },
    })
}
