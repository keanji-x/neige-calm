//! Deterministic ids for Track assistant conversations.
//!
//! `POST /api/tracks/{track_id}/conversations` lazily mints a card on its first
//! message. Both ids below are pure functions of `(track_id, Idempotency-Key)`,
//! so every retry aims at the same card even when operation dedup misses.

use sha2::{Digest, Sha256};

/// The pair of ids one `(track, Idempotency-Key)` pair derives.
pub(crate) struct DerivedConversationKeys {
    /// The conversation card id never carries the `#N` retry suffix; only the
    /// operation key changes when a terminally failed attempt is retried.
    pub(crate) card_id: String,
    pub(crate) operation_key: String,
}

/// The hash input retains its historical `wave-conversation` namespace.
///
/// That literal is persisted hash input. #1316 deliberately did not rename it:
/// changing it would make retries derive different card ids from rows already
/// stored by older builds.
pub(crate) fn derive_track_conversation_keys(
    track_id: &str,
    idempotency_key: &str,
) -> DerivedConversationKeys {
    let mut hasher = Sha256::new();
    hasher.update(format!("wave-conversation:{track_id}:{idempotency_key}"));
    let digest = hex::encode(hasher.finalize());
    DerivedConversationKeys {
        card_id: format!("conv-{}", &digest[..32]),
        operation_key: format!("wave-conversation-{digest}"),
    }
}

/// Exposed for tests that must construct the exact id accepted by the adapter.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn derive_track_conversation_card_id_for_test(track_id: &str, idempotency_key: &str) -> String {
    derive_track_conversation_keys(track_id, idempotency_key).card_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_derived_card_id_depends_only_on_track_and_idempotency_key() {
        let derived = derive_track_conversation_keys("track-1", "key-a");
        assert_eq!(derived.card_id, "conv-55cef7267426fe78493bdd46ca6b1220");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-55cef7267426fe78493bdd46ca6b12203ac64772d7a4b869d9e51bf764e2529c"
        );

        let other = derive_track_conversation_keys("track-1", "key-b");
        assert_ne!(other.card_id, derived.card_id);
        let other_track = derive_track_conversation_keys("track-2", "key-a");
        assert_ne!(other_track.card_id, derived.card_id);
    }
}
