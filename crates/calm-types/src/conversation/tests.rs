use super::*;
use crate::harness::HarnessProfile;
use crate::model::HarnessInputPresentation;
use crate::observation::Observation;
use serde_json::json;
use std::num::NonZeroU32;

const ID: &str = "0123456789abcdef0123456789abcdef";

fn human() -> SubmissionRenderInput {
    SubmissionRenderInput::Human {
        message: MessageRevisionRef {
            message_id: ConversationMessageId::parse(ID).unwrap(),
            revision: NonZeroU32::new(1).unwrap(),
        },
        author: FrozenHumanAttribution {
            registered_user_id: "alice".into(),
            display_name: "Alice".into(),
        },
    }
}

#[test]
fn conversation_ids_validate_at_construction_and_decode_without_reminting() {
    let id = InputSubmissionId::parse(ID).unwrap();
    assert_eq!(serde_json::to_value(&id).unwrap(), json!(ID));
    assert_eq!(
        serde_json::from_value::<InputSubmissionId>(json!(ID)).unwrap(),
        id
    );
    for invalid in [
        "",
        "uuid",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcdeg",
    ] {
        assert!(InputSubmissionId::parse(invalid).is_err(), "{invalid}");
        assert!(
            serde_json::from_value::<InputAttemptId>(json!(invalid)).is_err(),
            "{invalid}"
        );
    }
    assert!(serde_json::from_value::<InputAttempt>(json!({"submission_id": ID})).is_err());
    assert!(
        serde_json::from_value::<MessageRevisionRef>(json!({"message_id": ID,"revision": 0}))
            .is_err()
    );
}

#[test]
fn render_plan_has_human_references_and_requires_exact_ordered_claims() {
    let input = human();
    let segment = SubmissionRenderSegment::Input {
        input_position: 0,
        input: input.clone(),
    };
    let plan = SubmissionRenderPlan::V1 {
        segments: vec![segment.clone()],
    };
    assert_eq!(plan.input_refs().unwrap(), vec![input.input_ref()]);
    let wire = serde_json::to_value(&plan).unwrap();
    assert_eq!(wire["segments"][0]["input"]["message"]["message_id"], ID);
    assert!(wire["segments"][0]["input"].get("text").is_none());
    assert!(wire["segments"][0]["input"].get("payload").is_none());
    assert_eq!(
        serde_json::from_value::<SubmissionRenderPlan>(wire.clone()).unwrap(),
        plan
    );
    let mut copied_body = wire;
    copied_body["segments"][0]["input"]["text"] = json!("must not be archived");
    assert!(serde_json::from_value::<SubmissionRenderPlan>(copied_body).is_err());
    let mut segments = vec![segment];
    segments.push(SubmissionRenderSegment::Input {
        input_position: 1,
        input,
    });
    assert!(SubmissionRenderPlan::V1 { segments }.input_refs().is_err());
    assert!(
        SubmissionRenderPlan::V1 {
            segments: vec![SubmissionRenderSegment::Input {
                input_position: 1,
                input: human()
            }]
        }
        .input_refs()
        .is_err()
    );
    assert!(
        SubmissionRenderPlan::V1 { segments: vec![] }
            .input_refs()
            .is_err()
    );
}

#[test]
fn system_payload_cannot_deserialize_legacy_human_text_or_user_attribution() {
    assert!(
        SystemObservation::try_from(Observation::UserMessage {
            text: "human".into()
        })
        .is_err()
    );
    assert!(
        serde_json::from_value::<SystemObservation>(json!({"type":"user_message","text":"human"}))
            .is_err()
    );
    assert!(SystemRenderContext::new(HarnessInputPresentation::User, "human".into()).is_err());
    assert!(
        serde_json::from_value::<SystemRenderContext>(
            json!({"presentation":"user","text":"human"})
        )
        .is_err()
    );
    let system = SystemObservation::try_from(Observation::SystemContext {
        text: "kernel".into(),
    })
    .unwrap();
    assert_eq!(
        serde_json::from_value::<SystemObservation>(serde_json::to_value(&system).unwrap())
            .unwrap(),
        system
    );
    let erased = json!({"state":"erased_by_private_history_operation","operation_id":ID});
    let payload: PrivateHistoryPayload<SystemObservation> =
        serde_json::from_value(erased.clone()).unwrap();
    assert_eq!(serde_json::to_value(payload).unwrap(), erased);
    let mut hidden_copy = erased;
    hidden_copy["value"] = json!({"type":"system_context","text":"removed"});
    assert!(
        serde_json::from_value::<PrivateHistoryPayload<SystemObservation>>(hidden_copy).is_err()
    );
}

#[test]
fn moved_harness_profile_preserves_persisted_operation_names() {
    for (profile, name) in [
        (HarnessProfile::Planner, "spec"),
        (HarnessProfile::PlainChat, "plain_chat"),
        (HarnessProfile::Assistant, "assistant"),
    ] {
        assert_eq!(serde_json::to_value(profile).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<HarnessProfile>(json!(name)).unwrap(),
            profile
        );
    }
}

#[test]
fn journal_v2_proof_binds_exact_attempt_refs_and_original_digest_without_payload() {
    let plan = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: human(),
        }],
    };
    let attempt = InputAttempt {
        submission_id: InputSubmissionId::parse(ID).unwrap(),
        attempt_id: InputAttemptId::parse("11111111111111111111111111111111").unwrap(),
    };
    let digest = SubmissionRenderDigest::for_plan(&plan).unwrap();
    let proof =
        SubmissionDispatchProof::from_stored_plan(attempt.clone(), &plan, digest.clone()).unwrap();
    assert_eq!(proof.attempt(), &attempt);
    assert_eq!(proof.render_plan().sha256, digest);
    let wire = serde_json::to_value(&proof).unwrap();
    assert_eq!(wire["version"], "v2");
    assert!(wire.get("input").is_none());
    assert!(wire.get("prompt").is_none());
    assert!(wire.get("queue").is_none());
    assert!(wire.get("segments").is_none());
    assert_eq!(
        serde_json::from_value::<SubmissionDispatchProof>(wire.clone()).unwrap(),
        proof
    );
    let mut mismatch = wire.clone();
    mismatch["render_plan"]["submission_id"] = json!("22222222222222222222222222222222");
    assert!(serde_json::from_value::<SubmissionDispatchProof>(mismatch).is_err());
    let mut duplicate_identity = wire.clone();
    let mut second_revision = duplicate_identity["input_refs"][0].clone();
    second_revision["message"]["revision"] = json!(2);
    duplicate_identity["input_refs"]
        .as_array_mut()
        .unwrap()
        .push(second_revision);
    assert!(serde_json::from_value::<SubmissionDispatchProof>(duplicate_identity).is_err());
    let mut hidden_body = wire;
    hidden_body["prompt"] = json!("must not be archived");
    assert!(serde_json::from_value::<SubmissionDispatchProof>(hidden_body).is_err());
    let changed = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::Human {
                message: MessageRevisionRef {
                    message_id: ConversationMessageId::parse(ID).unwrap(),
                    revision: NonZeroU32::new(2).unwrap(),
                },
                author: FrozenHumanAttribution {
                    registered_user_id: "alice".into(),
                    display_name: "Alice".into(),
                },
            },
        }],
    };
    assert!(SubmissionDispatchProof::from_stored_plan(attempt, &changed, digest).is_err());
}
