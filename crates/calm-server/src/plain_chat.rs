use crate::model::{Card, CardRole};

/// Whether a card carries the persisted plain-chat marker; with `require_worker_codex` the marker is only authoritative on a Worker `codex` card.
pub(crate) fn card_is_plain_chat(
    card: &Card,
    role: Option<CardRole>,
    require_worker_codex: bool,
) -> bool {
    let marked = card
        .payload
        .get("harness_profile")
        .and_then(serde_json::Value::as_str)
        == Some(crate::harness::profile::PLAIN_CHAT_MARKER);
    marked && (!require_worker_codex || (card.kind == "codex" && role == Some(CardRole::Worker)))
}

/// Whether a card carries the persisted track-assistant marker. Never conflate with the plain-chat predicate: an assistant holds a token that reaches the block channel, a plain chat has no track authority.
pub(crate) fn card_is_track_assistant(
    card: &Card,
    role: Option<CardRole>,
    require_assistant_codex: bool,
) -> bool {
    let marked = card
        .payload
        .get("harness_profile")
        .and_then(serde_json::Value::as_str)
        == Some(crate::harness::profile::ASSISTANT_MARKER);
    marked
        && (!require_assistant_codex || (card.kind == "codex" && role == Some(CardRole::Assistant)))
}

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
            Some(CardRole::Planner),
            true
        ));
        assert!(!card_is_plain_chat(
            &card("terminal"),
            Some(CardRole::Worker),
            true
        ));
    }
}
