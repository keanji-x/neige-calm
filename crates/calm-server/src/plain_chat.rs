use crate::model::{Card, CardRole};

/// Whether a card carries the persisted plain-chat marker.
///
/// `require_worker_codex` is for harness-facing boundaries, where the marker
/// is only authoritative on a Worker `codex` card. Callers that merely need
/// to suppress spec-only machinery (for example worker-flow attachment) pass
/// `false` so malformed/legacy marked rows still fail closed.
pub(crate) fn card_is_plain_chat(
    card: &Card,
    role: Option<CardRole>,
    require_worker_codex: bool,
) -> bool {
    let marked = card
        .payload
        .get("harness_profile")
        .and_then(serde_json::Value::as_str)
        == Some("plain_chat");
    marked && (!require_worker_codex || (card.kind == "codex" && role == Some(CardRole::Worker)))
}

/// Whether a card carries the persisted track-assistant marker (#1189).
///
/// Same shape as [`card_is_plain_chat`] and same fail-closed reading of
/// `require_assistant_codex`, but a *different* marker value and a different
/// role. The two must never be conflated: a plain chat has no MCP token and no
/// track authority at all, while an assistant holds a token that reaches the
/// block channel. Answering one question with the other's predicate would
/// either strand the assistant outside the harness routes or hand an area chat
/// the assistant's surface.
pub(crate) fn card_is_track_assistant(
    card: &Card,
    role: Option<CardRole>,
    require_assistant_codex: bool,
) -> bool {
    let marked = card
        .payload
        .get("harness_profile")
        .and_then(serde_json::Value::as_str)
        == Some(crate::operation::spec_harness_start_adapter::ASSISTANT_HARNESS_PROFILE_MARKER);
    marked
        && (!require_assistant_codex || (card.kind == "codex" && role == Some(CardRole::Assistant)))
}

/// Whether a card is either flavour of lazily minted conversation.
///
/// Used where the question is "is this a headless conversation card rather than
/// a worker the worker-flow machinery owns?" — the answer is the same for both
/// flavours and the distinction between them is irrelevant there.
pub(crate) fn card_is_lazy_conversation(card: &Card) -> bool {
    card_is_plain_chat(card, None, false) || card_is_track_assistant(card, None, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CardId, TrackId};
    use serde_json::json;

    fn card(kind: &str) -> Card {
        Card {
            id: CardId::from("card-chat"),
            track_id: TrackId::from("track-chat"),
            title: None,
            kind: kind.into(),
            sort: 0.0,
            payload: json!({"harness_profile": "plain_chat"}),
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn assistant_card(kind: &str) -> Card {
        Card {
            payload: json!({"harness_profile": "assistant"}),
            ..card(kind)
        }
    }

    /// The two markers are disjoint in both directions. Without this, a
    /// widened predicate ("any harness_profile marker") would let an area chat
    /// card answer yes to the assistant question — and the assistant question
    /// is what opens the MCP-backed harness routes.
    #[test]
    fn the_two_conversation_markers_never_answer_for_each_other() {
        assert!(card_is_track_assistant(
            &assistant_card("codex"),
            Some(CardRole::Assistant),
            true
        ));
        assert!(!card_is_plain_chat(
            &assistant_card("codex"),
            Some(CardRole::Assistant),
            false
        ));
        assert!(!card_is_track_assistant(
            &card("codex"),
            Some(CardRole::Worker),
            false
        ));
        // Role and kind still constrain the strict reading.
        assert!(!card_is_track_assistant(
            &assistant_card("codex"),
            Some(CardRole::Worker),
            true
        ));
        assert!(!card_is_track_assistant(
            &assistant_card("terminal"),
            Some(CardRole::Assistant),
            true
        ));
        // The union predicate accepts both and nothing else.
        assert!(card_is_lazy_conversation(&assistant_card("codex")));
        assert!(card_is_lazy_conversation(&card("codex")));
        assert!(!card_is_lazy_conversation(&Card {
            payload: json!({}),
            ..card("codex")
        }));
    }

    #[test]
    fn optional_shape_constraint_is_explicit() {
        assert!(card_is_plain_chat(&card("codex"), None, false));
        assert!(card_is_plain_chat(
            &card("codex"),
            Some(CardRole::Worker),
            true
        ));
        assert!(!card_is_plain_chat(
            &card("codex"),
            Some(CardRole::Spec),
            true
        ));
        assert!(!card_is_plain_chat(
            &card("terminal"),
            Some(CardRole::Worker),
            true
        ));
    }
}
