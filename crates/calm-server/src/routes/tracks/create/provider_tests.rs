//! #1791: `planner_provider` enters the create digest only when it is not Codex.

use super::*;
use crate::session_projection_repo::AgentProvider;

fn provider_shape(planner_provider: AgentProvider) -> CreateRequestShape {
    CreateRequestShape {
        planner_provider,
        model: None,
        reasoning_effort: None,
        title: "shared cwd".into(),
        sort: None,
        cwd: Some("/repo".into()),
        template_id: None,
        recipe_id: None,
        template_input: None,
        attach_folder: false,
        allow_cross_area_cwd: None,
        theme: RequestTheme {
            fg: (1, 2, 3),
            bg: (4, 5, 6),
        },
        fork_report_from: None,
    }
}

/// Independently computed: SHA-256 of the sorted-key compact JSON of the create payload as
/// it was before `planner_provider` existed. Every binding on disk (4140: 38 fingerprinted
/// ones) predates the field.
const PRE_PROVIDER_DIGEST: &str =
    "803decc34626516bed9b9e477a21b53f3294345398a2333408f08e389365e901";

fn binding(
    request_fingerprint: TrackCreateRequestFingerprint,
) -> crate::db::sqlite::TrackCreateBinding {
    crate::db::sqlite::TrackCreateBinding {
        track_id: "track".into(),
        planner_card_id: "planner".into(),
        report_card_id: "report".into(),
        request_fingerprint,
    }
}

/// A pre-provider binding keeps replaying for the identical Codex create, a legacy binding
/// keeps failing closed, and the same key with another provider is a payload conflict.
#[test]
fn a_codex_create_keeps_every_pre_provider_binding_and_another_provider_conflicts() {
    let codex = create_request_digest(&provider_shape(AgentProvider::Codex)).unwrap();
    let claude = create_request_digest(&provider_shape(AgentProvider::Claude)).unwrap();
    assert_eq!(
        codex, PRE_PROVIDER_DIGEST,
        "a Codex create changed its digest"
    );
    assert_ne!(claude, codex);

    let fingerprinted = binding(TrackCreateRequestFingerprint::V1 {
        create_request_sha256: PRE_PROVIDER_DIGEST.into(),
        first_message_sha256: "message".into(),
    });
    ensure_binding_create_matches(&fingerprinted, &codex, "key", CreateShape::WithFirstMessage)
        .expect("the identical Codex create replays its binding");
    let conflict = ensure_binding_create_matches(
        &fingerprinted,
        &claude,
        "key",
        CreateShape::WithFirstMessage,
    )
    .expect_err("another provider under the same key");
    assert!(matches!(conflict, CalmError::Conflict(_)), "{conflict:?}");

    let legacy = binding(TrackCreateRequestFingerprint::LegacyUnknown);
    for digest in [&codex, &claude] {
        let refused =
            ensure_binding_create_matches(&legacy, digest, "key", CreateShape::WithFirstMessage)
                .expect_err("a legacy binding fails closed");
        assert!(
            matches!(&refused, CalmError::Conflict(message)
                if message.contains("predates durable request fingerprints")),
            "{refused:?}"
        );
    }
}
