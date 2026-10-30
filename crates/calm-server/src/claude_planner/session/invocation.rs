//! Neutral invocation inputs enter the existing Claude host integration here.
use super::*;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use crate::harness::issuance::{IssuanceRefusal, SelectionSource};
use base64::Engine as _;
use calm_types::planner_attachment::AttachmentId;
use provider::claude::protocol::{Base64Image, UserLineContent, client_line_uuid};
use provider::control::*;

impl ClaudePlannerSession {
    pub(crate) async fn start_turn(
        &self,
        thread: &str,
        request: TurnRequest,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
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
                track_fence,
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
        track_fence: &crate::provider_execution::TrackFenceHandoff,
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
            track_fence,
        )
        .await
    }

    /// Dry-run under the original issue lock, so no turn spawns beside the check.
    pub(crate) async fn prepare_edit(
        &self,
        thread: &str,
        target: NativeEditTarget,
        track_fence: &crate::provider_execution::TrackAdmissionFence,
    ) -> Result<EditPreparation<CalmError>> {
        let cut = truncation(target)?;
        let _issue = self.shared.issue.lock().await;
        let busy = {
            let state = self.shared.state();
            state.shutting_down || state.active.is_some() || self.shared.parked.is_parked()
        };
        if busy {
            return Err(CalmError::Conflict(
                "the Claude conversation is busy".into(),
            ));
        }
        let params = &self.shared.params;
        if let Some(hold) = self.identity_hold() {
            return Err(CalmError::Conflict(hold));
        }
        let start = self.shared.state().start;
        if start == SessionStart::New
            && matches!(
                super::super::identity::launch(params).await?,
                super::super::identity::Identity::Unconfirmed
            )
        {
            return Err(CalmError::Conflict(
                "Claude session launch is unconfirmed; start a fresh session to continue".into(),
            ));
        }
        self.provider_admission_gate().await?;
        super::super::rewind::check(params, thread, &cut, track_fence).await
    }

    /// All starts call this while holding this session's issue lock.
    pub(super) async fn provider_admission_gate(&self) -> Result<()> {
        let inactive = self.shared.state().active.is_none();
        if inactive {
            let _ = super::execution_end::join_driver(&self.shared).await;
        }
        let params = &self.shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        params
            .host
            .provider_execution
            .admission_gate(&pool, params.engine.as_ref(), &params.worker_session_id)
            .await
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

/// Text blocks, and one base64 image block per bound attachment (§5.6); the attachment's file name
/// is its id, which names its format.
async fn user_line_content(items: &[InputItem]) -> Result<Vec<UserLineContent>> {
    let mut content = Vec::with_capacity(items.len());
    for item in items {
        match item {
            InputItem::Text { text } => content.push(UserLineContent::Text { text: text.clone() }),
            InputItem::LocalImage { path } => {
                let id = std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| AttachmentId::parse(name).ok())
                    .ok_or_else(|| {
                        CalmError::BadRequest(format!("{path} is not a bound attachment"))
                    })?;
                let bytes = tokio::fs::read(path).await?;
                content.push(UserLineContent::Image {
                    source: Base64Image::new(
                        id.format().mime(),
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ),
                });
            }
        }
    }
    Ok(content)
}
