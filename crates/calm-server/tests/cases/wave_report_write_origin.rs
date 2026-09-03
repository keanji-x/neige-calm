//! #1252 S1 step 1 — the module-external half of the `WritePolicy`
//! encapsulation check.
//!
//! `WritePolicy`'s fields are private, so `policy_for` is the only way to get
//! one. This file lives outside the `calm_server` crate entirely, which is the
//! strictest vantage point available: it can name the type, so a struct
//! literal here would be a genuine attempt, and it fails to compile
//! (`error[E0451]: field ... is private`).
//!
//! What is asserted below is only that the accessors are the working route.
//! Everything about the *values* is declared intent — the comparison against
//! production's existing `(actor, author, auto_promote_draft,
//! recorder_shadow)` quadruples is S1 step 2's job.

use calm_server::error::CalmError;
use calm_server::ids::{ActorId, AreaId, CardId, WaveId};
use calm_server::wave_report_origin::{
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
    // A struct literal here — `WritePolicy { actor: ActorId::User, .. }` — is
    // rejected by the compiler, not by a runtime assertion. The positive half
    // is that the accessors work and are sufficient.
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
        wave_id: WaveId::from("w_1".to_string()),
        area_id: AreaId::from("ar_1".to_string()),
    })
}

/// The two lists driving the product below are not assumed complete. Each
/// helper labels its values through a `match` with **no `_` arm**, so adding a
/// variant to `CardRole` or `AgentProvider` fails to compile there and brings
/// the author into this file; the assertion then pins how many distinct labels
/// the list yields, which catches a list that lost or duplicated an entry. That
/// count is written out by hand and has to be bumped by hand.
fn assert_role_list_is_complete(roles: &[CardRole]) {
    fn label(role: CardRole) -> &'static str {
        match role {
            CardRole::Spec => "Spec",
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

/// Totality, over the origins this file can actually build — which is *every*
/// `WriteOrigin` variant, not just the unit-ish ones: `AgentOrigin`'s fields
/// are public, so an out-of-crate caller can name it, and `TrustedInitiator`'s
/// constructor is public too. No `WriteOrigin::Test` / `for_test` back door
/// exists; these are the production constructors.
///
/// Each origin is asserted to land on a *stated* outcome, not merely to avoid
/// one error kind: `Ok` for the four writable agent shapes plus `RestUser` and
/// `Fork`, and `Forbidden` for the four refused agent shapes. A `policy_for`
/// that started returning `Internal`, or that started refusing a writable
/// origin, fails here either way.
///
/// The agent half is the **whole `CardRole` × `AgentProvider` product**, all
/// eight combinations, built from the two variant lists below rather than
/// spelled out — `Worker`/`ReportCard` are refused on both providers, not just
/// on `Codex`. Today `policy_for` decides on the role alone, so the provider
/// axis is redundant *for the current implementation*; enumerating it is what
/// makes this test notice an implementation that stops being role-only, e.g.
/// one that let `Claude` through for a role `Codex` is refused on.
#[test]
fn policy_for_is_total_over_the_origins_a_caller_can_build_here() {
    let roles = [
        CardRole::Spec,
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
                CardRole::Spec | CardRole::Assistant => {
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
