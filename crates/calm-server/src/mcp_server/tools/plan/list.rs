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
                "reason" | "failure" | "blocking_reason" | "status_detail" | "failing_step"
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
            "access",
            "start",
            "status_detail",
            "task_projection",
            "worktree/path",
            "worktree/branch",
            "worktree/last_commit",
            "worktree/base_sha",
            "worktree/state",
            "candidate/binding",
            "candidate/reason",
            "candidate/delivery/state",
            "candidate/delivery/delivery_id",
            "candidate/delivery/ordinal",
            "candidate/delivery/candidate_id",
            "candidate/delivery/commit_sha",
            "candidate/delivery/failure/code",
            "candidate/verification/state",
            "candidate/verification/gate_attempt",
            "candidate/upstream/sha",
            "candidate/upstream/behind",
            "gate_result/passed",
            "gate_result/status",
            "gate_result/failing_step",
            "gate_result/exit_code",
            "gate_result/status_detail",
        ],
    );
    // An absent machine verdict remains an explicit null.
    if result.get("gate_result").is_none() {
        result["gate_result"] = Value::Null;
    }
    let mut omitted_fields = Vec::new();
    omitted(entry, &result, "", &mut omitted_fields);
    // Keep these paths in sync with task_list_entry; registry tests compare both views.
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
