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
use calm_server::ids::{ActorId, CardId, WaveId};
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
    })
}

/// Totality, over the origins this file can actually build — which is *every*
/// `WriteOrigin` variant, not just the unit-ish ones: `AgentOrigin`'s fields
/// are public, so an out-of-crate caller can name it, and `TrustedInitiator`'s
/// constructor is public too. No `WriteOrigin::Test` / `for_test` back door
/// exists; these are the production constructors.
///
/// Each origin is asserted to land on a *stated* outcome, not merely to avoid
/// one error kind: `Ok` for the four writable agent shapes plus `RestUser` and
/// `Fork`, and `Forbidden` for the two agent roles that may not write the
/// report. A `policy_for` that started returning `Internal`, or that started
/// refusing a writable origin, fails here either way.
#[test]
fn policy_for_is_total_over_the_origins_a_caller_can_build_here() {
    let expect_ok = [
        agent(CardRole::Spec, AgentProvider::Codex),
        agent(CardRole::Spec, AgentProvider::Claude),
        agent(CardRole::Assistant, AgentProvider::Codex),
        agent(CardRole::Assistant, AgentProvider::Claude),
        WriteOrigin::RestUser,
        WriteOrigin::Fork(user_fork()),
    ];
    for origin in expect_ok {
        policy_for(&origin)
            .unwrap_or_else(|error| panic!("{origin:?} must have a policy, got {error:?}"));
    }

    let expect_forbidden = [
        agent(CardRole::Worker, AgentProvider::Codex),
        agent(CardRole::ReportCard, AgentProvider::Codex),
    ];
    for origin in expect_forbidden {
        let error = policy_for(&origin).expect_err("worker/report-card may not write the report");
        assert!(
            matches!(error, CalmError::Forbidden(_)),
            "{origin:?}: expected Forbidden, got {error:?}"
        );
    }
}
