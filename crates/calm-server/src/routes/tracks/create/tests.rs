//! The keyed create's pure decisions: the request digest, the operation-key namespace, the arm
//! table and the outcome mapping.

use super::delivery::response_for;
use super::*;
use crate::operation::OperationOutcome;
use crate::session_projection_repo::AgentProvider;

#[test]
fn cross_area_authorization_is_part_of_idempotent_create_identity() {
    let shape = |allow_cross_area_cwd| CreateRequestShape {
        planner_provider: AgentProvider::Codex,
        model: None,
        reasoning_effort: None,
        title: "shared cwd".into(),
        sort: None,
        cwd: Some("/repo".into()),
        template_id: None,
        recipe_id: None,
        template_input: None,
        attach_folder: false,
        allow_cross_area_cwd,
        theme: RequestTheme {
            fg: (1, 2, 3),
            bg: (4, 5, 6),
        },
        fork_report_from: None,
    };
    let denied = create_request_digest(&shape(None)).unwrap();
    let allowed = create_request_digest(&shape(Some(super::super::CrossAreaCwdAuthorization {
        folder_id: 7,
        area_id: "owner".into(),
    })))
    .unwrap();
    assert_ne!(
        denied, allowed,
        "authorization must require a distinct idempotency key"
    );
}

/// Golden, not a round trip: a self-consistency check would stay green if the namespace
/// were merged into the conversation flavours'.
#[test]
fn the_track_create_key_is_a_pure_function_of_area_and_idempotency_key() {
    let key = derive_track_create_operation_key("area-1", "key-a");
    assert_eq!(
        key,
        // Independently computed: `sha256("track-create:area-1:key-a")`.
        "track-create-1c14cc746b371ade3520c32701cb2ff76e25a1bab237884e200a7d528c7af95f"
    );
    assert_ne!(key, derive_track_create_operation_key("area-1", "key-b"));
    assert_ne!(key, derive_track_create_operation_key("area-2", "key-a"));
}

/// The namespace separation, asserted by feeding ONE literal id to both derivations.
#[test]
fn the_track_create_namespace_never_collides_with_a_conversation_key() {
    let create = derive_track_create_operation_key("id-1", "key-a");
    let track = crate::conversation_keys::derive_track_conversation_keys("id-1", "key-a");
    assert_ne!(create, track.operation_key);
}

/// [`SelectedArm`]'s table, cell by cell.
#[test]
fn the_arm_is_decided_by_the_binding_then_by_what_sits_on_the_chosen_key() {
    let table = [
        // (binding_hit, chosen_is_occupied, expected)
        (false, false, SelectedArm::Mint),
        (false, true, SelectedArm::BindingLost),
        (true, true, SelectedArm::Replay),
        (true, false, SelectedArm::GenuineRetry),
    ];
    for (binding_hit, occupied, want) in table {
        assert_eq!(
            select_arm(binding_hit, occupied),
            want,
            "binding_hit={binding_hit} occupied={occupied}"
        );
    }
}

/// A collision outcome is a success only on a resuming arm. Constructed directly: the
/// variant is globally unreachable, so there is no integration construction.
#[test]
fn a_collision_outcome_is_a_success_only_on_a_resume_arm() {
    let collision = || OperationOutcome::SucceededViaCollision {
        existing_op_id: "op-1".to_string(),
        result: serde_json::json!({}),
    };
    let plain = || OperationOutcome::Succeeded {
        result: serde_json::json!({}),
    };
    assert!(response_for(SubmitArm::Mint, plain()).is_ok());
    for arm in [SubmitArm::Replay, SubmitArm::GenuineRetry] {
        assert!(response_for(arm, collision()).is_ok(), "{arm:?}");
        assert!(response_for(arm, plain()).is_ok(), "{arm:?}");
    }
    let refused = response_for(SubmitArm::Mint, collision())
        .expect_err("a fresh key cannot collide with itself");
    assert!(
        matches!(refused, CalmError::Internal(_)),
        "the mint arm must fail closed, not answer 201 for a delivery it did not make: \
         {refused:?}"
    );
}
