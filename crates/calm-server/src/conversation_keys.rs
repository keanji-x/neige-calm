//! Deterministic conversation ids, for both conversation flavours.
//!
//! Two endpoints mint a conversation card lazily on its first message:
//!
//! * `POST /api/coves/{cove_id}/conversations` — a cove chat, keyed on
//!   `(cove_id, Idempotency-Key)` (#1098 slice 3).
//! * `POST /api/waves/{wave_id}/conversations` — a wave assistant, keyed on
//!   `(wave_id, Idempotency-Key)` (#1189 slice 3).
//!
//! Both derivations are pure functions of their `(scope, key)` pair, which is
//! what makes a retry land on the same card even when operation dedup misses:
//! `SpecHarnessStartAdapter::validate` refuses to re-mint an existing card, so
//! every attempt under one key aims at one id.
//!
//! # The two namespaces must not collide
//!
//! They are separate string prefixes on purpose. A cove id and a wave id are
//! drawn from the same id space, so a shared prefix would let one
//! `(id, key)` pair address one card from two endpoints — a cove chat POST
//! could adopt (or block) a wave assistant card and vice versa. `G1` in the
//! design's gate table mutates `wave-conversation:` back to
//! `cove-chat-conversation:` precisely to prove this separation is load-bearing
//! rather than decorative.
//!
//! # The cove formula is frozen
//!
//! `derive_cove_conversation_keys` is byte-for-byte what `cove_conversations.rs`
//! shipped in #1098 and is mirrored by the frontend's `coveConversationCardId`
//! (`fe/core/domain/conversation.ts`), which is pinned by a golden. Moving the
//! function here must not change a single byte of what it computes; the golden
//! below travelled with it unchanged.
//!
//! The frontend's **wave** derivation is #1189 slice 5's job. Its formula is
//! spelled out in [`derive_wave_conversation_keys`] so that slice has an exact
//! specification to mirror.

use sha2::{Digest, Sha256};

/// The pair of ids one `(scope, Idempotency-Key)` pair derives.
pub(crate) struct DerivedConversationKeys {
    /// The conversation card's id. Never carries the `#N` retry suffix — that
    /// suffix touches only the operation key, which is what keeps "a retry can
    /// never mint a second conversation" true.
    pub(crate) card_id: String,
    /// The base operation idempotency key. `retryable_operation_key` may append
    /// `#N` to this after a terminally failed attempt.
    pub(crate) operation_key: String,
}

/// `SHA-256("cove-chat-conversation:{cove_id}:{key}")` — #1098's formula,
/// unchanged. See the module docs: the frontend mirrors this one and a golden
/// pins it on both sides.
pub(crate) fn derive_cove_conversation_keys(
    cove_id: &str,
    idempotency_key: &str,
) -> DerivedConversationKeys {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "cove-chat-conversation:{cove_id}:{idempotency_key}"
    ));
    let digest = hex::encode(hasher.finalize());
    DerivedConversationKeys {
        card_id: format!("conv-{}", &digest[..32]),
        operation_key: format!("cove-chat-conversation-{digest}"),
    }
}

/// `SHA-256("wave-conversation:{wave_id}:{key}")` — #1189 §4.4.
///
/// The digest feeds **both** ids: the card is `conv-{digest[..32]}` and the
/// operation key is `wave-conversation-{digest}`. Slice 5's frontend derivation
/// must reproduce the card id exactly: lower-case hex of the SHA-256 of that
/// UTF-8 string, first 32 hex characters, prefixed `conv-`.
///
/// The `conv-` prefix is deliberately the same as the cove flavour's: the
/// namespace inside the hashed string is what separates them, and keeping the
/// visible prefix identical is what makes G1's mutation (swap the namespace,
/// keep everything else) actually collide instead of merely producing a
/// differently-shaped id.
pub(crate) fn derive_wave_conversation_keys(
    wave_id: &str,
    idempotency_key: &str,
) -> DerivedConversationKeys {
    let mut hasher = Sha256::new();
    hasher.update(format!("wave-conversation:{wave_id}:{idempotency_key}"));
    let digest = hex::encode(hasher.finalize());
    DerivedConversationKeys {
        card_id: format!("conv-{}", &digest[..32]),
        operation_key: format!("wave-conversation-{digest}"),
    }
}

/// The wave flavour's card id, for tests that must construct a **correct** id
/// in order to prove that an incorrect one is what gets rejected.
///
/// Exposed rather than duplicated in the test: a test that recomputed the
/// formula itself would keep passing after a namespace change, which is exactly
/// the drift G1 exists to catch.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn derive_wave_conversation_card_id_for_test(wave_id: &str, idempotency_key: &str) -> String {
    derive_wave_conversation_keys(wave_id, idempotency_key).card_id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-CHAT-013(b)'s load-bearing wall, pinned as a golden. Moved verbatim
    /// from `routes::cove_conversations` when the derivation was shared with the
    /// wave flavour — the asserted values are unchanged, which is the proof the
    /// move did not disturb the frozen formula.
    ///
    /// The card id is a pure function of `(cove_id, Idempotency-Key)` and of
    /// NOTHING else — no nonce, no timestamp, and in particular **no `#N`
    /// operation-key suffix**. That is what makes "a retry can never mint a
    /// second conversation" hold even when operation dedup does not fire:
    /// `validate` refuses to re-mint a card that already exists, and every
    /// attempt under one key aims at the same id.
    ///
    /// It is a golden rather than a self-consistency check on purpose: a
    /// round-trip assertion (`derive(a) == derive(a)`) stays green if a nonce
    /// is added to a memoized derivation, and the retry paths mask a broken
    /// derivation behind the operation dedup's payload-hash 409 — so nothing
    /// else in the suite fails for the right reason.
    ///
    /// That the `#N` retry suffix cannot reach the card id is a signature-level
    /// fact, not something a test could falsify: `derive_cove_conversation_keys`
    /// takes `(cove_id, idempotency_key)` and has no parameter a suffix could
    /// arrive through. A test asserting "the id contains no `#`" would pass
    /// under every mutation and is deliberately not written.
    #[test]
    fn the_derived_card_id_depends_only_on_cove_and_idempotency_key() {
        let derived = derive_cove_conversation_keys("cove-1", "key-a");
        assert_eq!(derived.card_id, "conv-7b12bb251f95129865ab81128125cbf5");
        assert_eq!(
            derived.operation_key,
            "cove-chat-conversation-7b12bb251f95129865ab81128125cbf589c0a1e9e03c880294fa795f1e0f675f"
        );

        // Different key, different conversation — the derivation must not
        // collapse distinct requests onto one card either.
        let other = derive_cove_conversation_keys("cove-1", "key-b");
        assert_ne!(other.card_id, derived.card_id);
        // Same key on another cove is another conversation.
        let other_cove = derive_cove_conversation_keys("cove-2", "key-a");
        assert_ne!(other_cove.card_id, derived.card_id);
    }

    /// The wave flavour's golden, and slice 5's specification. A frontend
    /// implementation that reproduces these two strings is correct; one that
    /// does not is wrong, whatever it computes.
    #[test]
    fn the_derived_card_id_depends_only_on_wave_and_idempotency_key() {
        let derived = derive_wave_conversation_keys("wave-1", "key-a");
        assert_eq!(derived.card_id, "conv-9778c6de9be6196b5b44fdd411e5c305");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-9778c6de9be6196b5b44fdd411e5c3055809f7bdf2f1dba608b92a477faa723f"
        );

        let other = derive_wave_conversation_keys("wave-1", "key-b");
        assert_ne!(other.card_id, derived.card_id);
        let other_wave = derive_wave_conversation_keys("wave-2", "key-a");
        assert_ne!(other_wave.card_id, derived.card_id);
    }

    /// The namespace separation, asserted directly rather than left implicit in
    /// two goldens that happen to differ.
    ///
    /// The same `(id, key)` pair fed to both flavours must not produce the same
    /// card id. This is the unit-level twin of G1: it fails the moment someone
    /// "simplifies" the two namespaces into one shared prefix, without needing
    /// a booted server to notice.
    #[test]
    fn the_two_namespaces_never_derive_the_same_card_id() {
        let cove = derive_cove_conversation_keys("id-1", "key-a");
        let wave = derive_wave_conversation_keys("id-1", "key-a");
        assert_ne!(
            cove.card_id, wave.card_id,
            "a cove chat and a wave assistant must never address one card"
        );
        assert_ne!(cove.operation_key, wave.operation_key);
    }
}
