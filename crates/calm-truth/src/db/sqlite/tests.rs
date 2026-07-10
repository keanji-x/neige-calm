use super::{derive_session_identity, is_sqlite_busy_code};
use crate::session_projection_repo::WorkerSessionKind;
use calm_types::worker::{SessionMode, WorkerContract, WorkerProviderKind};

#[test]
fn sqlite_busy_code_matches_primary_and_extended_codes() {
    for code in ["5", "6", "261", "262", "517", "SQLITE_BUSY_SNAPSHOT"] {
        assert!(is_sqlite_busy_code(code), "code {code}");
    }
    for code in ["0", "1", "SQLITE_CONSTRAINT"] {
        assert!(!is_sqlite_busy_code(code), "code {code}");
    }
}

#[test]
fn derive_session_identity_frozen_table_satisfies_0045_checks() {
    let cases = [
        (
            WorkerSessionKind::Terminal,
            (
                WorkerProviderKind::Terminal,
                SessionMode::Ephemeral,
                WorkerContract::Executor,
            ),
        ),
        (
            WorkerSessionKind::CodexCard,
            (
                WorkerProviderKind::Codex,
                SessionMode::Resumable,
                WorkerContract::Executor,
            ),
        ),
        (
            WorkerSessionKind::ClaudeCard,
            (
                WorkerProviderKind::Claude,
                SessionMode::Ephemeral,
                WorkerContract::Executor,
            ),
        ),
        (
            WorkerSessionKind::SharedSpec,
            (
                WorkerProviderKind::Codex,
                SessionMode::Resumable,
                WorkerContract::Planner,
            ),
        ),
    ];

    for (kind, expected) in cases {
        let actual = derive_session_identity(&kind);
        assert_eq!(actual, expected, "kind {kind:?}");
        assert!(matches!(
            actual.0.as_db_str(),
            "codex" | "claude" | "terminal"
        ));
        assert!(matches!(actual.1.as_db_str(), "ephemeral" | "resumable"));
        assert!(matches!(
            actual.2.as_db_str(),
            "planner" | "executor" | "validator"
        ));
    }
}
