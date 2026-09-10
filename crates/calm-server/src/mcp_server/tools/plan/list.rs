//! Compact read projection only; qualification remains in the domain readers.
use super::*;

pub(super) struct Args {
    pub summary: bool,
    pub key: Option<String>,
}

impl Args {
    pub fn parse(args: &Value) -> Result<Self, RpcError> {
        let invalid = || {
            RpcError::invalid_params(
                "plan_list: expected optional detail=summary|full and a nonblank exact key; null and unknown arguments are invalid",
            )
        };
        let object = args.as_object().ok_or_else(invalid)?;
        if object.keys().any(|key| key != "detail" && key != "key") {
            return Err(invalid());
        }
        let summary = match object.get("detail") {
            None => false,
            Some(Value::String(detail)) if detail == "full" => false,
            Some(Value::String(detail)) if detail == "summary" => true,
            _ => return Err(invalid()),
        };
        let key = match object.get("key") {
            None => None,
            Some(Value::String(key)) if !key.trim().is_empty() => Some(key.clone()),
            _ => return Err(invalid()),
        };
        Ok(Self { summary, key })
    }
}

/// Copy exact public facts by path. No value here is a new eligibility decision.
fn select(source: &Value, paths: &[&str]) -> Value {
    let mut result = json!({});
    for path in paths {
        let parts: Vec<_> = path.split('/').collect();
        let mut value = source;
        for part in &parts {
            let Some(child) = value.get(*part) else {
                value = &Value::Null;
                break;
            };
            value = child;
        }
        // Distinguish an absent path from a present null fact.
        if source.pointer(&format!("/{path}")).is_none() {
            continue;
        }
        let mut destination = &mut result;
        for part in &parts[..parts.len() - 1] {
            if destination.get(*part).is_none() {
                destination[*part] = json!({});
            }
            destination = &mut destination[*part];
        }
        destination[parts[parts.len() - 1]] = value.clone();
    }
    result
}

fn omitted(source: &Value, result: &Value, prefix: &str, paths: &mut Vec<String>) {
    if let Some(fields) = source.as_object() {
        for (key, value) in fields {
            let path = format!("{prefix}/{key}");
            match result.get(key) {
                None => paths.push(path),
                Some(selected) => omitted(value, selected, &path, paths),
            }
        }
    }
}

fn bound_diagnostics(value: &mut Value, prefix: &str, paths: &mut Vec<String>) {
    if let Some(fields) = value.as_object_mut() {
        for (key, value) in fields {
            let path = format!("{prefix}/{key}");
            if matches!(
                key.as_str(),
                "reason"
                    | "failure"
                    | "blocking_reason"
                    | "status_detail"
                    | "preparation_failure"
                    | "authority_failure"
                    | "failing_step"
            ) && let Some(text) = value.as_str()
                && text.chars().count() > 256
            {
                *value = json!(text.chars().take(256).collect::<String>());
                paths.push(path.clone());
            }
            bound_diagnostics(value, &path, paths);
        }
    }
}

pub(super) fn summary(entry: &Value) -> Value {
    let mut result = select(
        entry,
        &[
            "key",
            "attempt_id",
            "generation",
            "status",
            "blocking_reason",
            "kind",
            "status_detail",
            "task_projection",
            "recovery/allowed",
            "recovery/code",
            "recovery/reason",
            "gate_result/passed",
            "gate_result/status",
            "gate_result/failing_step",
            "gate_result/exit_code",
            "gate_result/status_detail",
            "activity/as_of_ms",
            "activity/attempt_id",
            "activity/coverage",
            "activity/collector_health",
            "activity/interpretation",
            "activity/binding/operation_id",
            "activity/binding/conversation/path",
            "activity/binding/conversation/scope",
            "activity/binding/run/path",
            "activity/binding/run/scope",
            "activity/latest_recorded/row_id",
            "activity/latest_recorded/source_at_ms",
            "activity/latest_recorded/captured_at_ms",
            "activity/latest_recorded/detail/kind",
            "activity/latest_command_end/row_id",
            "activity/latest_command_end/source_at_ms",
            "activity/latest_command_end/captured_at_ms",
            "activity/latest_command_end/detail/kind",
            "activity/latest_command_end/detail/status",
            "activity/latest_command_end/detail/exit_code",
            "activity/tail_truncated",
        ],
    );
    if let Some(delivery) = entry.get("file_delivery").filter(|v| !v.is_null()) {
        result["file_delivery"] = select(
            delivery,
            &[
                "state",
                "failure",
                "scope",
                "qualified",
                "qualification/qualified",
                "qualification/reason",
                "authority_failure",
                "preparation_failure",
                "contract/role",
                "contract/producer",
                "contract/slot",
                "contract/purpose",
                "publication/operation_id",
                "publication/state",
                "publication/failure",
                "publication/reason",
                "candidate/state",
                "candidate/publication_operation_id",
                "candidate/snapshot",
                "verification/operation_id",
                "verification/state",
                "verification/failure",
                "verification/passed",
                "verification/failing_step",
                "verification/exit_code",
                "verification/status_detail",
                "review/state",
                "review/reviewer",
                "review/review_attempt_id",
                "review/review_operation_id",
                "review/report_event_id",
                "review/passed",
                "review/reason",
                "review/operation/state",
                "review/operation/failure",
                "decision/state",
                "decision/event_id",
                "decision/verdict_transaction",
                "decision/reason",
                "input/state",
                "input/producer_attempt_id",
                "input/publication_operation_id",
                "input/verification_operation_id",
                "input/decision_event_id",
                "input/purpose",
                "input/path",
                "repair/id",
                "repair/producer",
                "repair/source_attempt_id",
                "repair/repair_key",
                "repair/review_key",
                "repair/publication_operation_id",
                "repair/verification_operation_id",
                "repair/snapshot",
                "repair/review_attempt_id",
                "repair/report_event_id",
                "repair/stage",
                "repair/state",
                "repair/reason",
            ],
        );
    } else {
        result["file_delivery"] = Value::Null;
    }
    // Unknown activity evidence and absent machine verdicts remain explicit nulls.
    for path in ["gate_result", "activity"] {
        if result.get(path).is_none() {
            result[path] = Value::Null;
        }
    }
    let mut omitted_fields = Vec::new();
    omitted(entry, &result, "", &mut omitted_fields);
    // The lightweight constructor never copies these full-entry values. Keep
    // their paths in sync with task_list_entry; registry tests compare both views.
    // Even the full missing-projection fallback supplies id, but no task fields.
    omitted_fields.push("/id".into());
    if entry.get("task_projection").is_none() {
        omitted_fields.extend(
            [
                "/depends_on",
                "/priority",
                "/gate",
                "/worker_card_id",
                "/created_at_ms",
                "/finished_at_ms",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        omitted_fields.push(
            if entry["kind"] == "terminal" {
                "/command"
            } else {
                "/goal"
            }
            .into(),
        );
    }
    let mut truncated_fields = Vec::new();
    bound_diagnostics(&mut result, "", &mut truncated_fields);
    result["omitted_fields"] = json!(omitted_fields);
    result["truncated_fields"] = json!(truncated_fields);
    result["full_evidence"] =
        json!({"tool":TOOL_PLAN_LIST,"arguments":{"detail":"full","key":entry["key"]}});
    result
}

#[cfg(test)]
mod tests;
