//! Bounded sequential observations after reception has already timed out.
//! `sampling_completed` describes this collector, not operation completion or a deadline snapshot.
use super::{AppState, CanonicalHarnessSnapshot, Fixture};
use crate::operation::{Operation, OperationRepo, Phase, PhaseTag, SqlxOperationRepo};
use calm_truth::session_repo::SessionRepo;
use calm_types::worker::{WorkerSession, WorkerSessionId};
use serde_json::Value;
use std::time::Duration;

pub(super) async fn observe(
    fixture: &Fixture,
    state: &AppState,
    task: &str,
    worker: &str,
) -> String {
    let worker = WorkerSessionId(worker.to_owned());
    let mut registry_installed_after_timeout = "unavailable";
    let mut child = "unavailable".to_owned();
    let mut activation = "unavailable".to_owned();
    let mut session = "unavailable".to_owned();
    let operations = SqlxOperationRepo::new(fixture.repo.db().clone());
    // This budget only bounds diagnostics after the original 5s/10s receive limit has failed.
    let sampling_completed = tokio::time::timeout(Duration::from_secs(1), async {
        registry_installed_after_timeout = if state.harness.get(&worker.0).is_some() {
            "true"
        } else {
            "false"
        };
        child = operation_observation(
            state
                .operation_runtime
                .find_by_kind_and_idempotency(
                    crate::operation::child_track_adapter::CHILD_TRACK_KIND,
                    task,
                )
                .await,
        );
        match fixture.repo.session_get(&worker).await {
            Ok(Some(row)) => {
                let activation_id = row.spawn_op_id.clone();
                session = session_observation(row);
                activation = match activation_id {
                    Some(id) => operation_observation(operations.get_operation(&id).await),
                    None => "missing".to_owned(),
                };
            }
            Ok(None) => {
                session = "missing".to_owned();
                activation = "missing".to_owned();
            }
            Err(_) => {
                session = "unavailable".to_owned();
                activation = "unavailable".to_owned();
            }
        }
    })
    .await
    .is_ok();
    format!(
        "sampling_completed={sampling_completed} \
         registry_installed_after_timeout={registry_installed_after_timeout} \
         child_operation=({child}) activation_operation=({activation}) session=({session})"
    )
}

fn operation_observation(result: crate::error::Result<Option<Operation>>) -> String {
    let operation = match result {
        Ok(Some(operation)) => operation,
        Ok(None) => return "missing".to_owned(),
        Err(_) => return "unavailable".to_owned(),
    };
    let from_phase_source = match &operation.phase {
        Phase::Compensating => operation.compensation_state.as_ref(),
        _ => operation.phase_detail.as_ref(),
    };
    let from_phase = match from_phase_source {
        Some(Value::Object(detail)) => match detail.get("from_phase") {
            Some(value) => match serde_json::from_value::<PhaseTag>(value.clone()) {
                Ok(phase) => phase.as_str(),
                Err(_) => "invalid",
            },
            None => "missing",
        },
        Some(_) => "invalid",
        None => "missing",
    };
    let activation = match operation
        .tx_output
        .as_ref()
        .and_then(|output| output.data.get("activation"))
    {
        Some(value) => match value.as_str() {
            Some("prepared") => "prepared",
            Some("crossing_unknown") => "crossing_unknown",
            Some("native_known") => "native_known",
            Some("bound") => "bound",
            _ => "other",
        },
        None => "missing",
    };
    format!(
        "phase={} from_phase={from_phase} activation_marker={activation}",
        operation.phase.tag().as_str()
    )
}

fn session_observation(session: WorkerSession) -> String {
    let header_phase = match session.handle_state_json {
        Some(value) => match serde_json::from_value::<CanonicalHarnessSnapshot>(value) {
            Ok(snapshot) => format!("{:?}", snapshot.header.phase),
            Err(_) => "invalid".to_owned(),
        },
        None => "missing".to_owned(),
    };
    format!(
        "thread_id_present={} header_phase={header_phase}",
        session.thread_id.is_some()
    )
}

pub(super) fn method_label(method: Option<&str>) -> &'static str {
    match method {
        Some("config/read") => "config/read",
        Some("model/list") => "model/list",
        Some("thread/start") => "thread/start",
        Some("turn/start") => "turn/start",
        _ => "other",
    }
}
