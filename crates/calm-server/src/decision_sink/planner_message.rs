//! Admission of authoritative Planner records, independent of terminal notice delivery.
use super::{CardDecisionSink, CardDecisionSinkRecorderAdmissionGate};
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::error::CalmError;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::CardRole;
use crate::operation::worker_attempt::{current_running_attempt_tx, owned_attempt_tx};
use crate::recorder_admission::{RecorderAdmissionDecisionKind, RecorderAdmissionGate};

impl CardDecisionSink {
    /// Commit the record before a transport attempts its non-authoritative notice.
    pub(crate) async fn commit_planner_message(
        repo: &dyn RouteRepo,
        identity: &ToolCallIdentity,
        attempt_id: &str,
        worker_card: &str,
        key: &str,
        text: &str,
    ) -> Result<String, CalmError> {
        if identity.role != CardRole::Planner {
            return Err(CalmError::Forbidden(
                "Planner message requires Planner identity".into(),
            ));
        }
        let track = identity
            .track_id
            .clone()
            .ok_or_else(|| CalmError::Forbidden("Planner message requires a track".into()))?;
        let recorder =
            CardDecisionSinkRecorderAdmissionGate::for_identity(identity, track.clone().into());
        let (attempt, card, key, text) = (
            attempt_id.to_owned(),
            worker_card.to_owned(),
            key.to_owned(),
            text.to_owned(),
        );
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                recorder
                    .require_admission(tx, RecorderAdmissionDecisionKind::PlannerMessage)
                    .await?;
                let task = owned_attempt_tx(tx, &attempt, &card, &track).await?;
                current_running_attempt_tx(tx, &task).await?;
                Ok(crate::db::sqlite::planner_message_insert_tx(tx, &attempt, &key, &text).await?)
            })
        })
        .await
    }
}
