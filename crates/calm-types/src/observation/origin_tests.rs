//! The origin a turn-input segment records (#2206): which observation, from which event.

use serde_json::Value;

use super::{AnsweredQuestion, HookKind, MAIL_WAKE_SOURCE, Observation};
use crate::git_candidate::{DeliveryFailureCode, DeliverySettlement};
use crate::ids::{CardId, TrackId};
use crate::model::HarnessInputOrigin;

/// One value of every variant. The `match` has no wildcard, so a new variant fails to compile
/// until it gets an arm here, and the coverage assert then fails until it gets a sample.
fn one_of_every_variant() -> Vec<Observation> {
    let track_id = || TrackId::from("track-1");
    let card_id = || CardId::from("card-1");
    let samples = vec![
        Observation::TrackGoal { text: "g".into() },
        Observation::ReportEdited {
            track_id: track_id(),
            body_sha256: "sha".into(),
            body: "b".into(),
            author: None,
            body_before: None,
            doc_rev_after: None,
            blocks_after: None,
        },
        Observation::TaskCompleted {
            idempotency_key: "k".into(),
            result: Value::Null,
        },
        Observation::TaskFailed {
            idempotency_key: "k".into(),
            error: "e".into(),
        },
        Observation::WorkerHookStop {
            track_id: track_id(),
            card_id: card_id(),
            kind: HookKind::ClaudeStop,
            idempotency_key: String::new(),
        },
        Observation::SystemContext { text: "c".into() },
        Observation::UserMessage { text: "u".into() },
        Observation::TaskGateResult {
            idempotency_key: "k".into(),
            key: "k".into(),
            passed: true,
            failing_step: None,
            exit_code: None,
            log_tail: String::new(),
            attempt: 1,
            status_detail: None,
            target: None,
        },
        Observation::TaskGitDeliverySettled {
            key: "k".into(),
            attempt_id: "a".into(),
            result: DeliverySettlement::Failed {
                code: DeliveryFailureCode::WorkspaceMissing,
                reason: "r".into(),
                retry_allowed: false,
            },
            retained_path: None,
        },
        Observation::TrackWake {
            source: MAIL_WAKE_SOURCE.into(),
            key: "k".into(),
            text: "t".into(),
        },
        Observation::WorkspaceLeased {
            track_id: track_id(),
            card_id: card_id(),
            lease_id: "l".into(),
            path: "/p".into(),
        },
        Observation::WorkspaceReleased {
            track_id: track_id(),
            card_id: card_id(),
            lease_id: "l".into(),
        },
        Observation::ForgePrMerged {
            track_id: track_id(),
            pr_number: 1,
        },
        Observation::ForgeScanCompleted {
            track_id: track_id(),
            overlapping_prs: vec![],
        },
        Observation::ForgePrOpened {
            track_id: track_id(),
            pr_number: 1,
        },
        Observation::ForgePrChecks {
            track_id: track_id(),
            pr_number: 1,
            conclusion: "success".into(),
            snapshot: None,
            failed_checks: None,
        },
        Observation::ForgeIssueClosed {
            track_id: track_id(),
            issue_number: 1,
        },
        Observation::WorktreeProvisioned {
            track_id: track_id(),
            card_id: card_id(),
            path: "/p".into(),
        },
        Observation::WorktreeCommitted {
            track_id: track_id(),
            card_id: card_id(),
            commit_sha: "s".into(),
            branch: "b".into(),
        },
        Observation::AskAnswered {
            track_id: track_id(),
            answers: vec![AnsweredQuestion {
                title: "Merge?".into(),
                answer: "Merge".into(),
            }],
        },
    ];
    let arm = |observation: &Observation| match observation {
        Observation::TrackGoal { .. } => 0,
        Observation::ReportEdited { .. } => 1,
        Observation::TaskCompleted { .. } => 2,
        Observation::TaskFailed { .. } => 3,
        Observation::WorkerHookStop { .. } => 4,
        Observation::SystemContext { .. } => 5,
        Observation::UserMessage { .. } => 6,
        Observation::TaskGateResult { .. } => 7,
        Observation::TaskGitDeliverySettled { .. } => 8,
        Observation::TrackWake { .. } => 9,
        Observation::WorkspaceLeased { .. } => 10,
        Observation::WorkspaceReleased { .. } => 11,
        Observation::ForgePrMerged { .. } => 12,
        Observation::ForgeScanCompleted { .. } => 13,
        Observation::ForgePrOpened { .. } => 14,
        Observation::ForgePrChecks { .. } => 15,
        Observation::ForgeIssueClosed { .. } => 16,
        Observation::WorktreeProvisioned { .. } => 17,
        Observation::WorktreeCommitted { .. } => 18,
        Observation::AskAnswered { .. } => 19,
    };
    let covered = samples
        .iter()
        .map(arm)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(covered, (0..20).collect(), "a variant has no sample");
    samples
}

/// The origin names a segment's observation by the tag serde writes, so the two cannot drift.
#[test]
fn type_tag_is_the_serde_type_field_for_every_variant() {
    for observation in one_of_every_variant() {
        let wire = serde_json::to_value(&observation).unwrap();
        assert_eq!(
            wire["type"].as_str(),
            Some(observation.type_tag()),
            "{observation:?}"
        );
    }
}

#[test]
fn a_segment_names_its_observation_and_event() {
    for observation in one_of_every_variant() {
        let segment = observation.input_segment(Some(42));
        assert_eq!(
            segment.origin,
            Some(HarnessInputOrigin {
                observation: observation.type_tag().into(),
                event_id: Some(42),
            })
        );
    }
    let seed = Observation::TrackGoal { text: "g".into() }.input_segment(None);
    assert_eq!(seed.origin.and_then(|origin| origin.event_id), None);
}
