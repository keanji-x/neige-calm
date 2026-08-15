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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CardId, WaveId};
    use serde_json::json;

    fn card(kind: &str) -> Card {
        Card {
            id: CardId::from("card-chat"),
            wave_id: WaveId::from("wave-chat"),
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
