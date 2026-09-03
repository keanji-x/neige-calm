//! Deterministic conversation ids, for both conversation flavours.
//!
//! Two endpoints mint a conversation card lazily on its first message:
//!
//! * `POST /api/areas/{area_id}/conversations` — an area chat, keyed on
//!   `(area_id, Idempotency-Key)` (#1098 slice 3).
//! * `POST /api/tracks/{track_id}/conversations` — a track assistant, keyed on
//!   `(track_id, Idempotency-Key)` (#1189 slice 3).
//!
//! Both derivations are pure functions of their `(scope, key)` pair, which is
//! what makes a retry land on the same card even when operation dedup misses:
//! `SpecHarnessStartAdapter::validate` refuses to re-mint an existing card, so
//! every attempt under one key aims at one id.
//!
//! # The two namespaces must not collide
//!
//! They are separate string prefixes on purpose. An area id and a track id are
//! drawn from the same id space, so a shared prefix would let one
//! `(id, key)` pair address one card from two endpoints — an area chat POST
//! could adopt (or block) a track assistant card and vice versa. `G1` in the
//! design's gate table mutates `wave-conversation:` back to
//! `cove-chat-conversation:` precisely to prove this separation is load-bearing
//! rather than decorative.
//!
//! # The area formula is frozen
//!
//! `derive_area_conversation_keys` is byte-for-byte what `cove_conversations.rs`
//! shipped in #1098 — #1316 renamed that file to `area_conversations.rs`, but
//! "byte-for-byte" is a claim about the HASHED STRING, which #1316 explicitly
//! did not touch (see the function's doc comment) — and is mirrored by the
//! frontend's `areaConversationCardId`
//! (`fe/core/domain/conversation.ts`), which is pinned by a golden. Moving the
//! function here must not change a single byte of what it computes; the golden
//! below travelled with it unchanged.
//!
//! The frontend's **track** derivation is #1189 slice 5's job. Its formula is
//! spelled out in [`derive_track_conversation_keys`] so that slice has an exact
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

/// `SHA-256("cove-chat-conversation:{area_id}:{key}")` — #1098's formula,
/// unchanged.
///
/// **The two `cove-chat-conversation` literals below are DATA, not names, and
/// #1316 deliberately does not rename them.** They are hash INPUT. The digest
/// they produce is the conversation card's id (`conv-{digest[..32]}`, a
/// `cards.id` already persisted for every conversation ever created) and the
/// operation idempotency key (`cove-chat-conversation-{digest}`, already
/// persisted in `operations`). Change the input string and the same
/// `(area_id, key)` derives a DIFFERENT id: the server stops recognising rows
/// it minted itself, and — as the frontend mirror's golden puts it — the
/// symptom is not an error but a silence. Every existing conversation would be
/// orphaned and re-offered as new.
///
/// A migration cannot buy the rename back either: these ids are referenced
/// from `terminals.card_id`, `harness_items.card_id` and the operation log, so
/// rewriting them is a graph rewrite, not a column rename.
///
/// The identifier names around it (`derive_area_conversation_keys`, `area_id`)
/// ARE renamed — they are code, read by people. Only the hashed literal is
/// frozen. See the module docs: the frontend mirrors this one and a golden pins
/// it on both sides.
pub(crate) fn derive_area_conversation_keys(
    area_id: &str,
    idempotency_key: &str,
) -> DerivedConversationKeys {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "cove-chat-conversation:{area_id}:{idempotency_key}"
    ));
    let digest = hex::encode(hasher.finalize());
    DerivedConversationKeys {
        card_id: format!("conv-{}", &digest[..32]),
        operation_key: format!("cove-chat-conversation-{digest}"),
    }
}

/// `SHA-256("wave-conversation:{track_id}:{key}")` — #1189 §4.4.
///
/// **The two `wave-conversation` literals below are DATA, not names, and #1316
/// S2 deliberately does not rename them** — the same freeze 0080 applied to the
/// area flavour's `cove-chat-conversation:`. They are hash INPUT: the digest is
/// the conversation card's id (`conv-{digest[..32]}`, a `cards.id` already
/// persisted for every wave-assistant conversation ever created) and the
/// operation idempotency key (`wave-conversation-{digest}`, already in
/// `operations`). Change the input and the same `(track_id, key)` derives a
/// different id, so the server stops recognising rows it minted itself — and
/// the symptom is a silence, not an error: every existing conversation is
/// orphaned and re-offered as new. A migration cannot buy it back either;
/// these ids are referenced from `terminals.card_id`, `harness_items.card_id`
/// and the operation log, so rewriting them is a graph rewrite.
///
/// The identifiers around it (`derive_track_conversation_keys`, `track_id`) ARE
/// renamed — they are code, read by people. Only the hashed literal is frozen.
///
/// The digest feeds **both** ids: the card is `conv-{digest[..32]}` and the
/// operation key is `wave-conversation-{digest}`. Slice 5's frontend derivation
/// must reproduce the card id exactly: lower-case hex of the SHA-256 of that
/// UTF-8 string, first 32 hex characters, prefixed `conv-`.
///
/// The `conv-` prefix is deliberately the same as the area flavour's: the
/// namespace inside the hashed string is what separates them, and keeping the
/// visible prefix identical is what makes G1's mutation (swap the namespace,
/// keep everything else) actually collide instead of merely producing a
/// differently-shaped id.
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

/// The track flavour's card id, for tests that must construct a **correct** id
/// in order to prove that an incorrect one is what gets rejected.
///
/// Exposed rather than duplicated in the test: a test that recomputed the
/// formula itself would keep passing after a namespace change, which is exactly
/// the drift G1 exists to catch.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn derive_track_conversation_card_id_for_test(track_id: &str, idempotency_key: &str) -> String {
    derive_track_conversation_keys(track_id, idempotency_key).card_id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-CHAT-013(b)'s load-bearing wall, pinned as a golden. Moved verbatim
    /// from `routes::area_conversations` when the derivation was shared with the
    /// track flavour — the asserted values are unchanged, which is the proof the
    /// move did not disturb the frozen formula.
    ///
    /// The card id is a pure function of `(area_id, Idempotency-Key)` and of
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
    /// fact, not something a test could falsify: `derive_area_conversation_keys`
    /// takes `(area_id, idempotency_key)` and has no parameter a suffix could
    /// arrive through. A test asserting "the id contains no `#`" would pass
    /// under every mutation and is deliberately not written.
    #[test]
    fn the_derived_card_id_depends_only_on_area_and_idempotency_key() {
        let derived = derive_area_conversation_keys("area-1", "key-a");
        assert_eq!(derived.card_id, "conv-268c82584d9a20bce6719d19f019cd36");
        assert_eq!(
            derived.operation_key,
            "cove-chat-conversation-268c82584d9a20bce6719d19f019cd365e95f8ee8d4bf4c425af401ab3ba5cfd"
        );

        // Different key, different conversation — the derivation must not
        // collapse distinct requests onto one card either.
        let other = derive_area_conversation_keys("area-1", "key-b");
        assert_ne!(other.card_id, derived.card_id);
        // Same key on another area is another conversation.
        let other_area = derive_area_conversation_keys("area-2", "key-a");
        assert_ne!(other_area.card_id, derived.card_id);
    }

    /// The track flavour's golden, and slice 5's specification. A frontend
    /// implementation that reproduces these two strings is correct; one that
    /// does not is wrong, whatever it computes.
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

    /// The namespace separation, asserted directly rather than left implicit in
    /// two goldens that happen to differ.
    ///
    /// The same `(id, key)` pair fed to both flavours must not produce the same
    /// card id. It fails the moment someone "simplifies" the two namespaces
    /// into one shared prefix.
    ///
    /// G1 is pinned HERE, at the unit layer, because the route layer cannot
    /// construct the collision: the area endpoint hashes a `area_id` and the
    /// track endpoint hashes a `track_id`, and no request can make one id be the
    /// other. An end-to-end test would therefore be green under a merged
    /// namespace too — it would only ever exercise `(area-1, key)` against
    /// `(track-1, key)`, which differ whatever the prefix is. Feeding one
    /// literal id to both derivations is the only shape that actually
    /// distinguishes "separate namespaces" from "different inputs".
    #[test]
    fn the_two_namespaces_never_derive_the_same_card_id() {
        let area = derive_area_conversation_keys("id-1", "key-a");
        let track = derive_track_conversation_keys("id-1", "key-a");
        assert_ne!(
            area.card_id, track.card_id,
            "an area chat and a track assistant must never address one card"
        );
        assert_ne!(area.operation_key, track.operation_key);
    }
}
