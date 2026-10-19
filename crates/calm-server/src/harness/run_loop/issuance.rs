//! Ordinary issuance under the harness's existing claim and issuance lock.
use super::*;
use crate::harness::backend::StartDiagnostic;
use provider::control::{TurnEdit, TurnRequest, TurnStartOutcome};

pub(super) async fn maybe_issue_turn(inner: &Arc<Inner>) -> Result<()> {
    input_commit::resume(inner).await?;
    // Dev-forced harnesses run against the replay binary's stub app-server.
    if inner.issuance_paused.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Most ticks find the queue empty; bail before any logging so the 50ms
    // tick cadence does not flood the log with one entry line per tick.
    let (queue_len, only_report_edits, report_edits_carry_diffs) = {
        let queue = inner.pending_queue.lock().await;
        (
            queue.len(),
            queue_is_only_report_edits(&queue),
            queue_report_edits_all_carry_diffs(&queue),
        )
    };
    if queue_len == 0 {
        return Ok(());
    }
    if provider_holds_issuance(inner).await {
        return Ok(());
    }
    // Checked before any of the work below, because the point is to skip that work.
    if let Some(retry_after) = *inner.issuance_retry_after.lock().await
        && Instant::now() < retry_after
    {
        return Ok(());
    }
    let (hard_fire, first_pending_at, last_pending_at) = {
        let debounce = inner.debounce.lock().await;
        (
            debounce.hard_fire,
            debounce.first_pending_at,
            debounce.last_pending_at,
        )
    };
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        queue_len,
        hard_fire,
        "maybe_issue_turn entry (queue non-empty)"
    );

    let now = Instant::now();
    // A queue that is nothing but report edits waits for the editor to go quiet; `hard_fire` is
    // checked first and wins outright, so this only ever lengthens a wait.
    let (min_idle, max_wait) = if only_report_edits {
        (
            inner.config.report_edit_min_idle,
            inner.config.report_edit_max_wait,
        )
    } else {
        (
            inner.config.debounce_min_idle,
            inner.config.debounce_max_wait,
        )
    };
    let should_issue = if hard_fire {
        true
    } else {
        let Some(first) = first_pending_at else {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                hard_fire,
                "debounce gating turn issuance (no first_pending_at)"
            );
            return Ok(());
        };
        let Some(last) = last_pending_at else {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                hard_fire,
                "debounce gating turn issuance (no last_pending_at)"
            );
            return Ok(());
        };
        now.duration_since(last) >= min_idle || now.duration_since(first) >= max_wait
    };
    if !should_issue {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            runtime_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            hard_fire,
            first_pending_ms = first_pending_at
                .map(|t| now.duration_since(t).as_millis() as u64),
            last_pending_ms = last_pending_at
                .map(|t| now.duration_since(t).as_millis() as u64),
            "debounce gating turn issuance"
        );
        return Ok(());
    }

    {
        let state = inner.state.lock().await;
        if !state.can_issue_turn() {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                state = ?*state,
                "state gating turn issuance"
            );
            return Ok(());
        }
    }
    // The DURABLE half of "may I still speak for this card": a runtime can be retired in the
    // DATABASE with its run loop healthy and unaware. Placed above the work so a refused runtime
    // pays one indexed read per tick. The refusal does NOT wind the handle down.
    if !runtime_is_still_the_live_carrier(inner).await? {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            "runtime is no longer the card's live carrier; leaving the queue for its successor"
        );
        return Ok(());
    }
    // Refuse corrupt settings before transcript refresh/diff. Keep the queue intact and use
    // the same visible, paced refusal as other issuance prerequisites.
    let sampled_budget = match budget::sample(inner).await {
        Ok(budget) => budget,
        Err(failure) => {
            apply_refusal(inner, &failure).await;
            return Ok(());
        }
    };
    // Two independent per-turn decisions: `skip_transcript_refresh` skips the track-level WRITE
    // transaction; `skip_track_diff` issues with no since-last-turn block. An area chat skips both;
    // a track assistant skips only the refresh (it cannot read transcripts; the diff IS its context).
    // Do NOT extend the refresh skip to Planner or Worker cards — they would read a stale HEAD.
    let Some(card) = inner.repo.card_get(inner.card_id.as_str()).await? else {
        // A card that is gone has no conversation left to answer. The queue is not drained and the
        // state is not touched, so a card that reappears issues on the next tick.
        tracing::warn!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            "planner harness card is gone; not issuing a turn for it"
        );
        return Ok(());
    };
    let (skip_transcript_refresh, skip_track_diff) = {
        let role = inner.repo.card_role_get(card.id.as_str()).await?;
        if crate::plain_chat::card_is_plain_chat(&card, role, true) {
            (true, true)
        } else if crate::plain_chat::card_is_track_assistant(&card, role, true) {
            (true, false)
        } else {
            (false, false)
        }
    };
    let last_seen_head_snapshot = inner.last_seen_head.lock().await.clone();
    let refresh_head = if skip_transcript_refresh {
        None
    } else {
        let refresh_repo = Arc::clone(&inner.repo);
        let refresh_track_id = inner.track_id.clone();
        transcript_refresh_with_timeout(
            async move {
                let pool = refresh_repo.sqlite_db().ok_or_else(|| {
                    CalmError::Internal("transcript refresh requires a SQLite pool".into())
                })?;
                track_vcs::snapshot_transcripts_for_cards_in_track(
                    &pool,
                    &refresh_track_id,
                    None,
                    track_vcs::MANIFEST_SCHEMA_VERSION,
                )
                .await
                .map_err(CalmError::from)
            },
            TRANSCRIPT_REFRESH_TIMEOUT,
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
        )
        .await
    };
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        last_seen_head = ?last_seen_head_snapshot,
        refresh_head = ?refresh_head.as_deref(),
        "fetching since-last-turn diff"
    );
    // A batch of report edits each carrying its diff issues a turn whose input IS the diff, so the
    // since-last-turn block omits the unified patch; a pre-diff entry keeps it.
    let report_patch = if report_edits_carry_diffs {
        track_vcs::ReportPatch::Omit
    } else {
        track_vcs::ReportPatch::Include
    };
    let diff = if skip_track_diff {
        track_vcs::SinceLastTurnBlock::empty()
    } else {
        diff_with_timeout(inner, refresh_head.as_ref(), report_patch).await
    };
    // Deterministic drain-vs-supersede window. No-op in production.
    wait_at_planner_harness_drain_race_hook(inner).await;
    let _issuance_guard = inner.issuance.lock().await;
    // A lifecycle write holds this fence through its commit. Observations that
    // arrive later remain queued until the person explicitly restores the card.
    if inner
        .repo
        .conversation_archived_at(inner.card_id.as_str())
        .await?
        .is_some()
    {
        return Ok(());
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    if provider_holds_issuance(inner).await {
        return Ok(());
    }
    // Asked a SECOND time, immediately before the drain: a fence landing during the refresh and
    // diff above is exactly the case this is about.
    if !runtime_is_still_the_live_carrier(inner).await? {
        tracing::debug!(
            target: "calm_server::planner_harness_issue",
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            track_id = %inner.track_id,
            "runtime was retired while this turn was being prepared; leaving the queue"
        );
        return Ok(());
    }
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        block_some = diff.block.is_some(),
        current_head = ?diff.current_head.as_deref(),
        "since-last-turn diff resolved"
    );

    // A successful commit may have queued while the preceding Planner turn
    // was still accepting results. Re-evaluate at delivery, after that turn
    // can have marked Done; enqueue-time filtering would miss this case.
    consume_completed_worktree_commits(inner).await?;
    if inner.pending_queue.lock().await.is_empty() {
        return Ok(());
    }

    // The removed commit may have been the only hard-fire entry. Let the
    // next tick apply the existing debounce to any remaining soft entries;
    // this invocation was admitted using the queue's pre-consumption state.
    if hard_fire && !inner.debounce.lock().await.hard_fire {
        return Ok(());
    }

    // Resolve capability context before changing issuance state or draining the queue: a failed
    // grant read must leave the user's message available to retry. Both backends share this path.
    let workspace_report_instructions = crate::managed_track::workspace_report_instructions(
        inner.repo.as_ref(),
        inner.card_id.as_str(),
        inner.track_id.as_str(),
    )
    .await?;

    let timeout_notice = timeout_notice::previous(inner).await?;
    let prior_turn = {
        let mut state = inner.state.lock().await;
        if !state.can_issue_turn() {
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                state = ?*state,
                "state gating turn issuance post-diff"
            );
            return Ok(());
        }
        let prior = match &*state {
            HarnessState::TurnCompleted { last_turn_id } => Some(last_turn_id.clone()),
            _ => None,
        };
        *state = HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        };
        prior
    };
    *inner.issued_turn_id.lock().await = None;
    *inner.issued_turn_head.lock().await = None;
    *inner.interruption_intent.lock().await = None;
    // A new batch supersedes the legacy slot: by now that turn's echo has either consumed the slot
    // or is not coming.
    *inner.legacy_issued_input_segments.lock().await = None;
    // The key shared by the projection row and `turn/start`'s `clientUserMessageId`, decided HERE
    // so the snapshot written next carries it and a restart re-drains the batch under the SAME key.
    let client_id = {
        let from_queue = inner
            .pending_queue
            .lock()
            .await
            .iter()
            .find_map(QueueEntry::id)
            .cloned();
        let mut slot = inner.projection_client_id.lock().await;
        let key = slot
            .clone()
            .or(from_queue)
            .unwrap_or_else(crate::harness::queue::mint_entry_id);
        *slot = Some(key.clone());
        key
    };
    *inner.turn_budget.lock().await = match sampled_budget {
        Some(budget) => budget::FrozenBudget::Planner {
            budget,
            start_attempt: client_id.clone(),
            warning_attempted: false,
        },
        None => budget::FrozenBudget::NotApplicable,
    };
    persist_snapshot(inner).await?;

    let drained = {
        let mut queue = inner.pending_queue.lock().await;
        queue.drain(..).collect::<Vec<_>>()
    };
    if drained.is_empty() {
        *inner.state.lock().await = prior_turn
            .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
            .unwrap_or(HarnessState::Idle);
        *inner.issued_turn_id.lock().await = None;
        return Ok(());
    }
    *inner.debounce.lock().await = DebounceState::default();
    // Segments are built from the ENTRIES, not observations: an `Observation` cannot carry an attachment.
    let mut segments = input_segments_for_entries(&inner.card_id, &drained);
    crate::harness::result_receipt::enrich(
        inner.repo.as_ref(),
        &crate::state::WriteContext::new(
            inner.card_role_cache.clone(),
            inner.track_area_cache.clone(),
        ),
        &inner.track_id,
        &drained,
        &mut segments,
    )
    .await;

    // Entry-index consumers must finish before adding kernel-owned context.
    if let Some(budget) = sampled_budget {
        let mut context = budget::segment(budget.initial_input());
        context.presentation = HarnessInputPresentation::SystemPlannerTurnBudget;
        segments.insert(0, context);
    }
    if let Some(notice) = timeout_notice {
        segments.insert(0, notice);
    }

    // The channel statement is the batch's, appended once here; same flag as `report_patch`.
    append_report_edit_batch_channel_line(&mut segments, report_edits_carry_diffs);
    let joined_observation_text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        rebuffer_head(inner, drained).await;
        *inner.state.lock().await = HarnessState::PendingThreadStart;
        *inner.issued_turn_id.lock().await = None;
        persist_snapshot(inner).await?;
        return Ok(());
    };
    let drained_count = drained.len();
    let text = prepend_diff_block(diff.block.clone(), joined_observation_text);
    let text = match workspace_report_instructions {
        Some(instructions) => format!("{instructions}\n\n{text}"),
        None => text,
    };
    tracing::debug!(
        target: "calm_server::planner_harness_issue",
        runtime_id = %inner.worker_session_id,
        card_id = %inner.card_id,
        track_id = %inner.track_id,
        thread_id = %thread_id,
        drained_count,
        "calling backend.turn_start"
    );

    // The model and the permission mode are resolved HERE, as late as possible: the transcript
    // refresh and diff above can be tens of seconds, and a person who changed either inside that
    // window would otherwise watch the turn run under the one they just replaced.
    let resolved = async {
        let selection = inner.backend.resolve_selection(inner.as_ref()).await?;
        let permission = turn_permission::resolve(inner).await?;
        Ok::<_, IssuanceRefusal>((selection, permission))
    }
    .await;
    let (selection, permission) = match resolved {
        Ok(resolved) => resolved,
        Err(failure) => {
            // Not a wedge: `HarnessState::Wedged` has no exit in this tree, and wedging on a codex restart
            // ended the conversation permanently. But not every failure here clears itself, so the kinds
            // are separated below rather than all answered with a timer.
            tracing::warn!(
                target: "calm_server::planner_harness_issue",
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                reason = %failure.log,
                kind = ?failure.kind,
                "not issuing this turn: the model or permission mode to run it under is undetermined; will retry"
            );
            // The two arms differ in what waiting is worth, so they differ in how long we wait and in
            // whether the reader is told.
            apply_refusal(inner, &failure).await;
            rebuffer_head(inner, drained).await;
            #[cfg(feature = "fixtures")]
            inner.refused_issuances.fetch_add(1, Ordering::SeqCst);
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::Idle);
            *inner.issued_turn_id.lock().await = None;
            persist_snapshot(inner).await?;
            return Ok(());
        }
    };
    *inner.issuance_retry_after.lock().await = None;

    // Text first, then one `localImage` per attachment, in queue order. Every path was recorded and
    // verified at bind time; this builds no path and touches no disk, so a re-buffered batch is safe.
    let mut items = vec![InputItem::text(text)];
    items.extend(
        drained
            .iter()
            .flat_map(QueueEntry::attachments)
            .map(|attachment| InputItem::local_image(attachment.path.clone())),
    );
    // A projection row that could not be written is a LOCAL failure before the provider was asked; a
    // failed `turn/start` is classified by the backend that raised it. The log must not call the first
    // "turn/start failed".
    enum IssueFailure {
        ProjectionWrite(CalmError),
        Host(TurnStartFailure),
    }
    // A rewind's cut rides with whatever turn starts next; it is consumed only once one has. The
    // backend records in it what it already applied, which a failed start keeps below.
    let mut pending_rewind = inner.pending_rewind.lock().await.clone();
    let issued = async {
        // Written before `turn/start` goes out, so the row says what codex is told.
        write_projection_row(inner, &thread_id, client_id.as_str(), &segments)
            .await
            .map_err(IssueFailure::ProjectionWrite)?;
        let turn = IssueTurnHandle::from_reconciliation(inner)
            .await
            .turn_start(
                TurnRequest {
                    input: InputRequest {
                        items,
                        correlation: CorrelationKey(client_id.as_str().into()),
                    },
                    selection,
                    permission,
                    edit: pending_rewind
                        .as_ref()
                        .map_or(TurnEdit::NoEdit, BackendRewind::turn_edit),
                },
                &drained,
                pending_rewind.as_mut(),
            )
            .await
            .map_err(IssueFailure::Host)?;
        Ok::<_, IssueFailure>(turn)
    }
    .await;
    match issued {
        Ok(TurnStartOutcome::Started { turn }) => {
            let turn_id = turn.into_string();
            let incorporating = input_commit::retain_result(
                inner,
                input_commit::Continuation::IssueResolved {
                    client_id: client_id.clone(),
                    turn_id: Some(turn_id.clone()),
                },
            )
            .await;
            tracing::debug!(
                target: "calm_server::planner_harness_issue",
                runtime_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                track_id = %inner.track_id,
                thread_id = %thread_id,
                turn_id = %turn_id,
                "backend.turn_start ok"
            );
            // A turn that went out ends the run of refusals, so the notice and
            // the clock behind it both go with it.
            *inner.issuance_block.lock().await = None;
            *inner.refusing_since.lock().await = None;
            *inner.last_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_id.lock().await = Some(turn_id.clone());
            *inner.issued_turn_head.lock().await = diff.current_head.clone();
            // Cleared in the same snapshot that empties the queue, so no restart can pair this key with a later batch.
            *inner.projection_client_id.lock().await = None;
            // Known gap (#1923): a crash between the revert or start and the snapshot write below
            // replays the cut on restart. Codex then answers `turn not found`, which is refused
            // (#2512); the Claude CLI guard fails that one turn loudly.
            *inner.pending_rewind.lock().await = None;
            *inner.last_turn_base.lock().await = Some(TurnBase {
                turn_id: turn_id.clone(),
                seen_head: last_seen_head_snapshot.clone(),
            });
            drop(incorporating);
            input_commit::commit_issue(inner).await?;
        }
        failure @ (Err(
            IssueFailure::ProjectionWrite(_)
            | IssueFailure::Host(
                TurnStartFailure::Transient(_)
                | TurnStartFailure::Refused { .. }
                | TurnStartFailure::AwaitingRecovery { .. },
            ),
        )
        | Ok(
            TurnStartOutcome::Failed { .. }
            | TurnStartOutcome::Rejected { .. }
            | TurnStartOutcome::RecoveryRequired { .. },
        )) => {
            let incorporating = input_commit::retain_result(
                inner,
                input_commit::Continuation::IssueResolved {
                    client_id: client_id.clone(),
                    turn_id: None,
                },
            )
            .await;
            // Persisted below with the re-buffered batch. No replace runs meanwhile: it refuses
            // while this batch's projection key is set.
            *inner.pending_rewind.lock().await = pending_rewind;
            // Paced, like the refusal above: `rebuffer_head` arms `hard_fire`, and `PUT /planner/model`
            // can store a slug codex does not know, so an unpaced retry was twenty RPCs a second. The split
            // is the backend's TYPED verdict (#1981): `Rejected` because no choice is KNOWN to remove the need.
            let (e, stage, refusal) = match failure {
                Err(IssueFailure::ProjectionWrite(e)) => {
                    let stage = "projection row write failed before turn/start was sent";
                    let refusal = IssuanceRefusal::retryable(format!("{stage}: {e}"));
                    (e, stage, refusal)
                }
                Err(IssueFailure::Host(TurnStartFailure::Transient(e)))
                | Ok(TurnStartOutcome::Failed { error: e }) => {
                    let stage = "turn/start failed";
                    let refusal = IssuanceRefusal::retryable(format!("{stage}: {e}"));
                    (e, stage, refusal)
                }
                Err(IssueFailure::Host(TurnStartFailure::AwaitingRecovery { error, reader }))
                | Ok(TurnStartOutcome::RecoveryRequired {
                    error: StartDiagnostic { error, reader },
                }) => {
                    inner.provider_held.store(true, Ordering::SeqCst);
                    inner.provider_hold_owns_block.store(true, Ordering::SeqCst);
                    let stage = "provider awaits recovery";
                    let refusal = IssuanceRefusal::rejected(format!("{stage}: {error}"), reader);
                    (error, stage, refusal)
                }
                Err(IssueFailure::Host(TurnStartFailure::Refused { error, reader }))
                | Ok(TurnStartOutcome::Rejected {
                    error: StartDiagnostic { error, reader },
                }) => {
                    let stage = "turn/start failed";
                    let refusal = IssuanceRefusal::rejected(format!("{stage}: {error}"), reader);
                    (error, stage, refusal)
                }
                Ok(TurnStartOutcome::Started { .. }) => unreachable!("started turn handled above"),
            };
            apply_refusal(inner, &refusal).await;
            // Returning to editable input transfers authority with its next guarded snapshot.
            // A projection write can fail after insertion (during announcement), so even that
            // failure owes retirement of this batch key. The DELETE preserves confirmed history.
            rebuffer_head_retiring(inner, drained, std::slice::from_ref(&client_id)).await;
            #[cfg(feature = "fixtures")]
            inner.refused_issuances.fetch_add(1, Ordering::SeqCst);
            *inner.state.lock().await = prior_turn
                .map(|last_turn_id| HarnessState::TurnCompleted { last_turn_id })
                .unwrap_or(HarnessState::TurnCompleted {
                    last_turn_id: "unknown-turn".into(),
                });
            *inner.issued_turn_id.lock().await = None;
            *inner.issued_turn_head.lock().await = None;
            drop(incorporating);
            input_commit::commit_issue(inner).await?;
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                stage,
                error = %e,
                "planner harness could not issue the batch; re-buffered it"
            );
        }
    }
    Ok(())
}
