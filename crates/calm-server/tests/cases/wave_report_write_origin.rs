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
use calm_server::ids::ActorId;
use calm_server::wave_report_origin::{
    RecorderRequirement, TrustedInitiator, WriteAttribution, WriteOrigin, policy_for,
};
use calm_types::event::EditAuthor;

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
    let policy = policy_for(&WriteOrigin::Fork(TrustedInitiator::new(ActorId::User)))
        .expect("fork has a policy");
    assert_eq!(policy.attribution(), WriteAttribution::Structural);
    assert_eq!(policy.recorder(), RecorderRequirement::NotGated);
}

#[test]
fn policy_for_is_total_over_the_origins_a_caller_can_build_here() {
    // No `WriteOrigin::Test` / `for_test` back door exists; these are the
    // production constructors.
    for origin in [
        WriteOrigin::RestUser,
        WriteOrigin::Fork(TrustedInitiator::new(ActorId::User)),
    ] {
        let result = policy_for(&origin);
        assert!(
            !matches!(result, Err(CalmError::Internal(_))),
            "{origin:?} must not be an internal error"
        );
    }
}
