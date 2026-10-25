//! Derive worker liveness facts from authoritative Codex read responses.
pub fn liveness_facts_from_read(
    read: crate::codex::ThreadReadResponse,
    loaded: bool,
) -> crate::worker::CodexLivenessFacts {
    use crate::codex::{ThreadActiveFlag, ThreadStatus, TurnStatus};
    use crate::worker::{CodexLivenessFacts, LastTurnFacts, ThreadStatusLite, TurnStatusLite};

    let status = match read.thread.status {
        ThreadStatus::NotLoaded => ThreadStatusLite::NotLoaded,
        ThreadStatus::Idle => ThreadStatusLite::Idle,
        ThreadStatus::SystemError => ThreadStatusLite::SystemError,
        ThreadStatus::Active { active_flags } => ThreadStatusLite::Active {
            waiting_on_user_input: active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput),
            waiting_on_approval: active_flags.contains(&ThreadActiveFlag::WaitingOnApproval),
        },
    };
    // `last_turn`: None = no turns present (None or empty list).
    let last_turn = read
        .thread
        .turns
        .as_deref()
        .and_then(|turns| turns.last())
        .map(|turn| LastTurnFacts {
            cause: turn
                .error
                .as_ref()
                .map(crate::worker::WorkerFailureCause::from_codex),
            completed_at: turn.completed_at,
            status: match turn.status {
                TurnStatus::Completed => TurnStatusLite::Completed,
                TurnStatus::Interrupted => TurnStatusLite::Interrupted,
                TurnStatus::Failed => TurnStatusLite::Failed,
                TurnStatus::InProgress => TurnStatusLite::InProgress,
                TurnStatus::Unknown => TurnStatusLite::Unknown,
            },
        });
    CodexLivenessFacts {
        loaded,
        status,
        last_turn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn worker_failure_native_error_contract_and_unknown_are_preserved() {
        for (code, message, expected) in [
            (
                json!("other"),
                "Concurrency limit exceeded for user, please retry later",
                "concurrency_limit",
            ),
            (
                json!({"httpConnectionFailed":{"httpStatusCode":429}}),
                "Concurrency limit exceeded for user, please retry later",
                "concurrency_limit",
            ),
            (json!("cyberPolicy"), "refused", "content_refusal"),
            (json!("cyber_policy"), "refused", "provider_error"),
            (
                json!({"httpConnectionFailed":{"httpStatusCode":503}}),
                "Concurrency limit mentioned by a different error",
                "provider_error",
            ),
            (json!("other"), "not a concurrency limit", "provider_error"),
            (json!(null), "unknown", "provider_error"),
        ] {
            let read = serde_json::from_value(json!({"thread":{"status":{"type":"idle"},"turns":[{
                "status":"failed","completedAt":42,"error":{"message":message,"codexErrorInfo":code}
            }]}}))
            .unwrap();
            let facts = liveness_facts_from_read(read, true);
            let value = serde_json::to_value(facts.last_turn.unwrap().cause).unwrap();
            assert_eq!(value["kind"], expected);
            if code.is_object() {
                assert_eq!(value["native_code"], "httpConnectionFailed");
            } else {
                assert_eq!(value["native_code"], code);
            }
            assert_eq!(value["message"], message);
            assert_eq!(value["truncated"], false);
        }
    }

    #[test]
    fn worker_failure_native_error_bounds_preserve_unicode_prefixes() {
        for (message, expected_len, truncated) in [
            ("a".repeat(1024), 1024, false),
            ("a".repeat(1025), 1024, true),
            (format!("{}界", "a".repeat(1023)), 1023, true),
        ] {
            let read = serde_json::from_value(json!({"thread":{"status":{"type":"idle"},"turns":[{
                "status":"failed","error":{"message":message,"codexErrorInfo":"other"}
            }]}}))
            .unwrap();
            let cause = liveness_facts_from_read(read, true)
                .last_turn
                .unwrap()
                .cause
                .unwrap();
            assert_eq!(cause.message.len(), expected_len);
            assert!(message.starts_with(&cause.message));
            assert_eq!(cause.truncated, truncated);
        }
        let read = serde_json::from_value(json!({"thread":{"status":{"type":"idle"},"turns":[{
            "status":"failed","error":{"message":"detail","codexErrorInfo":"界".repeat(100)}
        }]}}))
        .unwrap();
        let cause = liveness_facts_from_read(read, true)
            .last_turn
            .unwrap()
            .cause
            .unwrap();
        assert_eq!(cause.native_code.unwrap().len(), 126);
        assert!(cause.truncated);
    }
}
