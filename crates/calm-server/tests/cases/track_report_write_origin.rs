//! `WritePolicy` encapsulation, checked from outside the `calm_server` crate.

use calm_server::error::CalmError;
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::track_report_origin::{
    AgentOrigin, RecorderRequirement, TrustedInitiator, WriteAttribution, WriteOrigin, policy_for,
};
use calm_types::event::EditAuthor;
use calm_types::model::CardRole;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionId;

fn user_fork() -> TrustedInitiator {
    TrustedInitiator::new(ActorId::User).expect("a user is a reachable fork initiator")
}

#[test]
fn a_write_policy_is_only_readable_through_its_accessors_from_outside_the_module() {
    // A struct literal here is rejected by the compiler (private fields), not by a runtime assertion.
    let policy = policy_for(&WriteOrigin::RestUser).expect("rest user has a policy");
    assert_eq!(policy.actor(), &ActorId::User);
    assert_eq!(
        policy.attribution(),
        WriteAttribution::Authored(EditAuthor::User)
    );
    assert!(!policy.auto_promote_draft());
    assert_eq!(policy.recorder(), RecorderRequirement::NotGated);
}

#[test]
fn fork_is_declared_structural_and_ungated_from_outside_the_module() {
    let policy = policy_for(&WriteOrigin::Fork(user_fork())).expect("fork has a policy");
    assert_eq!(policy.attribution(), WriteAttribution::Structural);
    assert_eq!(policy.recorder(), RecorderRequirement::NotGated);
}

fn agent(role: CardRole, provider: AgentProvider) -> WriteOrigin {
    WriteOrigin::Agent(AgentOrigin {
        card_id: CardId::from("c_1".to_string()),
        role,
        provider,
        session_id: WorkerSessionId::from("sess_1".to_string()),
        track_id: TrackId::from("w_1".to_string()),
    })
}

/// The `match` has no `_` arm, so a new variant fails to compile here; the count is bumped by hand.
fn assert_role_list_is_complete(roles: &[CardRole]) {
    fn label(role: CardRole) -> &'static str {
        match role {
            CardRole::Planner => "Planner",
            CardRole::Assistant => "Assistant",
            CardRole::Worker => "Worker",
            CardRole::ReportCard => "ReportCard",
        }
    }
    let mut labels: Vec<&'static str> = roles.iter().copied().map(label).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(
        labels.len(),
        4,
        "every `CardRole` must appear: got {labels:?}"
    );
}

fn assert_provider_list_is_complete(providers: &[AgentProvider]) {
    fn label(provider: &AgentProvider) -> &'static str {
        match provider {
            AgentProvider::Codex => "Codex",
            AgentProvider::Claude => "Claude",
        }
    }
    let mut labels: Vec<&'static str> = providers.iter().map(label).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(
        labels.len(),
        2,
        "every `AgentProvider` must appear: got {labels:?}"
    );
}

#[test]
fn policy_for_is_total_over_the_origins_a_caller_can_build_here() {
    let roles = [
        CardRole::Planner,
        CardRole::Assistant,
        CardRole::Worker,
        CardRole::ReportCard,
    ];
    let providers = [AgentProvider::Codex, AgentProvider::Claude];

    assert_role_list_is_complete(&roles);
    assert_provider_list_is_complete(&providers);

    for role in roles {
        for provider in providers.iter().cloned() {
            let origin = agent(role, provider);
            match role {
                CardRole::Planner | CardRole::Assistant => {
                    policy_for(&origin).unwrap_or_else(|error| {
                        panic!("{origin:?} must have a policy, got {error:?}")
                    });
                }
                CardRole::Worker | CardRole::ReportCard => {
                    let error = policy_for(&origin)
                        .expect_err("worker/report-card may not write the report");
                    assert!(
                        matches!(error, CalmError::Forbidden(_)),
                        "{origin:?}: expected Forbidden, got {error:?}"
                    );
                }
            }
        }
    }
    for origin in [WriteOrigin::RestUser, WriteOrigin::Fork(user_fork())] {
        policy_for(&origin)
            .unwrap_or_else(|error| panic!("{origin:?} must have a policy, got {error:?}"));
    }
}
