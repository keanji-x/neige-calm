//! Where a task execution runs.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

/// #1830 S2 D5: whether a task runs in its track's checkout — a codex or claude task that is not
/// on the child-track route. Such tasks share the checkout, so a task that changes it runs alone.
pub fn runs_in_track_checkout(kind: &str, spawn: &str) -> bool {
    matches!(kind, "codex" | "claude") && spawn != crate::task_recovery::TASK_CHILD_TRACK_ROUTE
}

/// #1917: what a task declares it does to the track's checkout (`tasks.access`, the task block's
/// `access`; absent = `read_write`). Read-only tasks may run beside each other; a task that changes
/// the checkout runs alone. Declared, not enforced: no sandbox changes with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskAccess {
    ReadOnly,
    ReadWrite,
}

impl TaskAccess {
    /// The column and block spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskAccess::ReadOnly => "read_only",
            TaskAccess::ReadWrite => "read_write",
        }
    }

    /// Every spelling, for error messages that name the valid choices.
    pub const CHOICES: &'static str = "\"read_only\" | \"read_write\"";
}

impl TryFrom<String> for TaskAccess {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "read_only" => Ok(TaskAccess::ReadOnly),
            "read_write" => Ok(TaskAccess::ReadWrite),
            other => Err(format!(
                "access {other:?} is not one of {}",
                TaskAccess::CHOICES
            )),
        }
    }
}

/// #1917: `access` is `"read_only"` or `"read_write"` (absent or null = `"read_write"`). A
/// read-only task is a codex or claude task in the track's checkout with no gate.
pub(crate) fn validate_task_access(map: &Map<String, Value>, errors: &mut Vec<String>) {
    let access = match map.get("access") {
        None | Some(Value::Null) => return,
        Some(value) => value
            .as_str()
            .map(|access| TaskAccess::try_from(access.to_string())),
    };
    match access {
        Some(Ok(TaskAccess::ReadWrite)) => {}
        Some(Ok(TaskAccess::ReadOnly)) => {
            if !matches!(
                map.get("kind").and_then(Value::as_str),
                Some("codex" | "claude")
            ) {
                errors.push(
                    "access: \"read_only\" requires kind \"codex\" or \"claude\"; a terminal task \
                     takes \"read_write\""
                        .into(),
                );
            }
            if map.get("spawn").and_then(Value::as_str)
                == Some(crate::task_recovery::TASK_CHILD_TRACK_ROUTE)
            {
                errors.push(format!(
                    "access: \"read_only\" runs in the track's checkout; it requires spawn {:?}, \
                     not {:?}",
                    crate::task_recovery::TASK_IN_TRACK_ROUTE,
                    crate::task_recovery::TASK_CHILD_TRACK_ROUTE
                ));
            }
            if map.get("gate").is_some_and(|gate| !gate.is_null()) {
                errors.push(
                    "access: \"read_only\" takes no gate; drop the gate, or declare \
                     \"read_write\" for a task that changes the checkout"
                        .into(),
                );
            }
        }
        _ => errors.push(format!("access: must be one of {}", TaskAccess::CHOICES)),
    }
}

/// #2058: where a task's checkout starts (`tasks.start`, the task block's `start`; absent =
/// `checkout`). `upstream`: the kernel fetches the track's upstream, starts the checkout there and
/// tells the worker to replay the track's last done commit (a catch-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStart {
    Checkout,
    Upstream,
}

impl TaskStart {
    /// The column and block spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStart::Checkout => "checkout",
            TaskStart::Upstream => "upstream",
        }
    }

    /// Every spelling, for error messages that name the valid choices.
    pub const CHOICES: &'static str = "\"checkout\" | \"upstream\"";
}

impl TryFrom<String> for TaskStart {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "checkout" => Ok(TaskStart::Checkout),
            "upstream" => Ok(TaskStart::Upstream),
            other => Err(format!(
                "start {other:?} is not one of {}",
                TaskStart::CHOICES
            )),
        }
    }
}

/// #2058 D4: `start` is `"checkout"` or `"upstream"` (absent or null = `"checkout"`). An upstream
/// start replays the track's work into the track's checkout: a codex or claude task on the
/// in-track route that changes the checkout.
pub(crate) fn validate_task_start(map: &Map<String, Value>, errors: &mut Vec<String>) {
    let start = match map.get("start") {
        None | Some(Value::Null) => return,
        Some(value) => value
            .as_str()
            .map(|start| TaskStart::try_from(start.to_string())),
    };
    match start {
        Some(Ok(TaskStart::Checkout)) => {}
        Some(Ok(TaskStart::Upstream)) => {
            if !matches!(
                map.get("kind").and_then(Value::as_str),
                Some("codex" | "claude")
            ) {
                errors.push(
                    "start: \"upstream\" requires kind \"codex\" or \"claude\"; a terminal task \
                     takes \"checkout\""
                        .into(),
                );
            }
            if map.get("access").and_then(Value::as_str) == Some(TaskAccess::ReadOnly.as_str()) {
                errors.push(
                    "start: \"upstream\" changes the checkout; it requires access \
                     \"read_write\", not \"read_only\""
                        .into(),
                );
            }
            if map.get("spawn").and_then(Value::as_str)
                == Some(crate::task_recovery::TASK_CHILD_TRACK_ROUTE)
            {
                errors.push(format!(
                    "start: \"upstream\" runs in the track's checkout; it requires spawn {:?}, \
                     not {:?}",
                    crate::task_recovery::TASK_IN_TRACK_ROUTE,
                    crate::task_recovery::TASK_CHILD_TRACK_ROUTE
                ));
            }
        }
        _ => errors.push(format!("start: must be one of {}", TaskStart::CHOICES)),
    }
}

/// #1933: a full commit id as git prints it, 40 lowercase hex digits.
fn is_full_commit_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// #1933: `head` (the commit the track checkout must be at when the task starts) and `base` (the
/// commit a review compares against) are full commit ids (absent or null = undeclared), declared
/// only on a read-only task.
pub(crate) fn validate_reader_commits(map: &Map<String, Value>, errors: &mut Vec<String>) {
    let read_only =
        map.get("access").and_then(Value::as_str) == Some(TaskAccess::ReadOnly.as_str());
    for field in ["head", "base"] {
        match map.get(field) {
            None | Some(Value::Null) => continue,
            Some(Value::String(id)) if is_full_commit_id(id) => {}
            Some(_) => errors.push(format!(
                "{field}: must be a full commit id (40 lowercase hex digits)"
            )),
        }
        if !read_only {
            errors.push(format!(
                "{field}: applies only to a codex or claude task with access: \"read_only\""
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskAccess;

    #[test]
    fn access_spelling_is_the_serde_spelling_and_round_trips() {
        for access in [TaskAccess::ReadOnly, TaskAccess::ReadWrite] {
            assert_eq!(
                serde_json::to_value(access).unwrap(),
                serde_json::Value::from(access.as_str())
            );
            assert_eq!(
                TaskAccess::try_from(access.as_str().to_string()),
                Ok(access)
            );
        }
        assert!(TaskAccess::try_from("readonly".to_string()).is_err());
    }

    #[test]
    fn start_spelling_is_the_serde_spelling_and_round_trips() {
        use super::TaskStart;
        for start in [TaskStart::Checkout, TaskStart::Upstream] {
            assert_eq!(
                serde_json::to_value(start).unwrap(),
                serde_json::Value::from(start.as_str())
            );
            assert_eq!(TaskStart::try_from(start.as_str().to_string()), Ok(start));
        }
        assert!(TaskStart::try_from("main".to_string()).is_err());
    }
}
