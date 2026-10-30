use super::{
    NativeSourceExclusion as Excluded, NativeSourceFact as Fact,
    NativeSourceMalformed as Malformed, native_source_fact,
};
use crate::db::sqlite::TrackCreateRequestFingerprint as Fingerprint;
use crate::error::CalmError;
use serde_json::json;

#[test]
fn u13_b2_p0_native_identity_preserves_exact_bytes() {
    for method in ["item/started", "item/completed"] {
        let fact = native_source_fact(
            "t\"\n雪",
            Some("i\\\t"),
            Some("agentMessage"),
            method,
            &json!({}),
        )
        .unwrap();
        let Fact::Identity(identity) = fact else {
            panic!("expected identity")
        };
        assert_eq!(identity, r#"["t\"\n雪","i\\\t"]"#);
        assert_eq!(
            super::presentation::native_source_identity(
                "t\"\n雪",
                Some("i\\\t"),
                Some("agentMessage"),
                method,
                &json!({})
            )
            .unwrap(),
            identity
        );
    }
}

#[test]
fn u13_b2_p0_native_exclusions_and_malformed_keep_conflict_contract() {
    let cases = [
        (
            "t",
            None,
            Some("agentMessage"),
            "item/completed",
            json!({}),
            Fact::Malformed {
                reason: Malformed::MissingItemIdentity,
            },
        ),
        (
            "t",
            Some(""),
            Some("agentMessage"),
            "item/completed",
            json!({}),
            Fact::Malformed {
                reason: Malformed::MissingItemIdentity,
            },
        ),
        (
            "",
            Some("i"),
            Some("agentMessage"),
            "item/completed",
            json!({}),
            Fact::Malformed {
                reason: Malformed::MissingThreadIdentity,
            },
        ),
        (
            "t",
            Some("i"),
            None,
            "item/completed",
            json!({}),
            Fact::Excluded {
                reason: Excluded::MissingItemType,
            },
        ),
        (
            "t",
            Some("i"),
            Some(""),
            "item/completed",
            json!({}),
            Fact::Excluded {
                reason: Excluded::MissingItemType,
            },
        ),
        (
            "t",
            Some("i"),
            Some("userMessage"),
            "item/completed",
            json!({}),
            Fact::Excluded {
                reason: Excluded::InputEcho,
            },
        ),
        (
            "t",
            Some("i"),
            Some("user_message"),
            "item/completed",
            json!({}),
            Fact::Excluded {
                reason: Excluded::InputEcho,
            },
        ),
        (
            "t",
            Some("i"),
            Some("agentMessage"),
            "item/completed",
            json!({"_projection":true}),
            Fact::Excluded {
                reason: Excluded::InputProjection,
            },
        ),
        (
            "t",
            Some("i"),
            Some("agentMessage"),
            "turn/completed",
            json!({}),
            Fact::Excluded {
                reason: Excluded::UnsupportedMethod,
            },
        ),
        // Preserve the original item/thread checks before exclusion.
        (
            "",
            None,
            Some("userMessage"),
            "other",
            json!({"_projection":true}),
            Fact::Malformed {
                reason: Malformed::MissingItemIdentity,
            },
        ),
    ];
    for (thread, item, kind, method, params, expected) in cases {
        assert_eq!(
            native_source_fact(thread, item, kind, method, &params).unwrap(),
            expected
        );
        assert!(matches!(
            super::presentation::native_source_identity(thread, item, kind, method, &params),
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }
    for projection in [json!(false), json!(null), json!(1), json!("true")] {
        assert!(matches!(
            native_source_fact(
                "t",
                Some("i"),
                Some("agentMessage"),
                "item/completed",
                &json!({"_projection":projection})
            )
            .unwrap(),
            Fact::Identity(_)
        ));
    }
}

#[test]
fn u13_b2_p0_fingerprint_versions_preserve_null_and_opaque_digests() {
    assert_eq!(
        Fingerprint::decode(0, None, None).unwrap(),
        Fingerprint::LegacyUnknown
    );
    assert_eq!(
        Fingerprint::decode(1, Some(" exact\n雪 ".into()), Some("".into())).unwrap(),
        Fingerprint::V1 {
            create_request_sha256: " exact\n雪 ".into(),
            first_message_sha256: "".into()
        }
    );
    assert_eq!(
        Fingerprint::decode(2, Some("".into()), None).unwrap(),
        Fingerprint::V2MessageLess {
            create_request_sha256: "".into()
        }
    );
    for version in [-1, 0, 1, 2, 3, i64::MAX] {
        for create in [None, Some("create".to_owned())] {
            for message in [None, Some("message".to_owned())] {
                if matches!(
                    (version, create.is_some(), message.is_some()),
                    (0, false, false) | (1, true, true) | (2, true, false)
                ) {
                    continue;
                }
                assert!(matches!(
                    Fingerprint::decode(version, create.clone(), message),
                    Err(CalmError::Internal(_))
                ));
            }
        }
    }
}
