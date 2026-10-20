//! Neutral invocation inputs enter the existing Claude host integration here.
use super::*;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use provider::control::*;

impl ClaudePlannerSession {
    pub(crate) async fn start_turn(
        &self,
        thread: &str,
        request: TurnRequest,
    ) -> Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
        let truncation = match request.edit {
            TurnEdit::NoEdit => None,
            TurnEdit::ApplyNativePlan(plan) => Some(truncation(plan.target)?),
        };
        let input = SpawnInput::Turn {
            items: request.input.items,
            client_id: &request.input.correlation.0,
            truncation: truncation.as_ref(),
        };
        start::start_result(
            self.start_spawn(
                thread,
                input,
                &request.selection,
                request.permission,
                &historical_admission(),
            )
            .await,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_canonical(
        &self,
        thread: &str,
        items: Vec<provider::InputItem>,
        client_id: &str,
        selection: &provider::TurnModelSelection,
        permission: PlannerPermissionMode,
        execution: crate::harness::backend::ExecutionInput,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<Uuid, SpawnFailure> {
        self.start_spawn(
            thread,
            SpawnInput::Canonical {
                items,
                client_id,
                execution,
            },
            selection,
            permission,
            admission,
        )
        .await
    }

    /// Dry-run under the original issue lock, so no turn spawns beside the check.
    pub(crate) async fn prepare_edit(
        &self,
        thread: &str,
        target: NativeEditTarget,
    ) -> Result<EditPreparation<CalmError>> {
        let cut = truncation(target)?;
        let _issue = self.shared.issue.lock().await;
        let busy = {
            let state = self.shared.state();
            state.shutting_down || state.active.is_some()
        };
        if busy {
            return Err(CalmError::Conflict(
                "the Claude conversation is busy".into(),
            ));
        }
        super::super::rewind::check(&self.shared.params, thread, &cut).await
    }

    pub(crate) async fn resolve_selection(
        &self,
        source: &impl SelectionSource,
    ) -> Result<TurnModelSelection, IssuanceRefusal> {
        if let Err(error) = self.host().configured() {
            return Err(IssuanceRefusal::needs_a_choice(
                error.to_string(),
                format!(
                    "{}. Your message is still queued and will be sent once the server runs with it.",
                    unavailable_message()
                ),
            ));
        }
        let payload = source.card_payload().await?;
        // No catalog read at issue: the CLI judges the stored selection.
        super::super::models::turn_selection(&payload)
            .map_err(|(log, reader)| IssuanceRefusal::needs_a_choice(log, reader))
    }

    pub(crate) async fn compact_request(
        &self,
        source: &impl SelectionSource,
    ) -> Result<CompactRequest> {
        use super::super::compact;
        if self.host().configured().is_err() {
            return Err(compact::refused(unavailable_message()));
        }
        let selection = self.resolve_selection(source).await.map_err(|refusal| {
            compact::refused(if refusal.reader.is_empty() {
                refusal.log
            } else {
                refusal.reader
            })
        })?;
        Ok(CompactRequest { selection })
    }
}

/// Lower the native target only at the Claude host boundary.
fn truncation(target: NativeEditTarget) -> Result<ResumeTruncation> {
    match target {
        NativeEditTarget::TranscriptCut {
            resume_at,
            drops_turn,
        } => Ok(ResumeTruncation {
            at: resume_at,
            drops_turn,
        }),
        NativeEditTarget::BeforeTurn { .. } => Err(CalmError::Internal(
            "an edit target names another provider".into(),
        )),
    }
}

/// Both invocation contracts encode input through the existing payload owner.
pub(super) async fn user_line(
    thread: Uuid,
    client_id: &str,
    items: &[provider::InputItem],
) -> Result<UserLine, SpawnFailure> {
    Ok(UserLine::new(
        thread,
        client_line_uuid(client_id)
            .map_err(|error| SpawnFailure::failed(CalmError::BadRequest(error.to_string())))?,
        user_line_content(items)
            .await
            .map_err(SpawnFailure::failed)?,
    ))
}
