//! The two notification sources of a track (#1829, #2209): an ask (an unanswered
//! `neige_user_ask`) and planner down (the Planner's newest finished turn failed). A pure function
//! of the rows `sql::notification_rows` read; nothing else is an item.

use std::collections::BTreeSet;

use calm_truth::readable_error_text::readable_error_text;
use serde::{Deserialize, Serialize};

use crate::event::AskQuestion;

/// The two key shapes: a source prefix, then the evidence row's id (the `ask.requested` event id
/// for an ask, the transcript row id for a failed turn).
const ASK_KEY: &str = "ask:";
const PLANNER_DOWN_KEY: &str = "planner_down:";

/// Whether `key` has one of the two item key shapes (`<prefix><decimal row id>`). The Dismiss
/// route admits only these; whether the item is still open is not asked.
pub fn is_item_key(key: &str) -> bool {
    [ASK_KEY, PLANNER_DOWN_KEY]
        .iter()
        .filter_map(|prefix| key.strip_prefix(prefix))
        .any(|id| id.bytes().all(|b| b.is_ascii_digit()) && id.parse::<i64>().is_ok())
}

/// What an item is: `ask` folds to `attention = input`, `planner_down` to `attention = failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotificationSource {
    Ask,
    PlannerDown,
}

/// One thing addressed to the user and not yet handled, tagged by `source`. `key` carries the
/// evidence row's id, so the same source happening again is a new key; `text` is the kernel's
/// words for it, shown verbatim. Only an ask carries its id and questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivityItem {
    Ask {
        key: String,
        /// The questions' titles joined by ` / `.
        text: String,
        at_ms: i64,
        ask_id: i64,
        questions: Vec<AskQuestion>,
    },
    PlannerDown {
        key: String,
        text: String,
        at_ms: i64,
    },
}

impl ActivityItem {
    pub fn source(&self) -> NotificationSource {
        match self {
            ActivityItem::Ask { .. } => NotificationSource::Ask,
            ActivityItem::PlannerDown { .. } => NotificationSource::PlannerDown,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            ActivityItem::Ask { key, .. } | ActivityItem::PlannerDown { key, .. } => key,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            ActivityItem::Ask { text, .. } | ActivityItem::PlannerDown { text, .. } => text,
        }
    }

    pub fn at_ms(&self) -> i64 {
        match self {
            ActivityItem::Ask { at_ms, .. } | ActivityItem::PlannerDown { at_ms, .. } => *at_ms,
        }
    }
}

/// N1 — one `ask.requested` of the track that no `ask.answered` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAsk {
    pub ask_id: i64,
    pub at_ms: i64,
    pub questions: Vec<AskQuestion>,
}

/// N3 — the Planner card's newest `turn/completed` row that is not `interrupted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTurn {
    pub row_id: i64,
    pub at_ms: i64,
    pub status: Option<String>,
    /// `$.error.message`.
    pub error_message: Option<String>,
}

/// Everything the two sources read. A track without a Planner card has no last turn and no U.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationRows {
    pub asks: Vec<OpenAsk>,
    /// U — the newest user message to the Planner card.
    pub user_sent_at: Option<i64>,
    pub last_turn: Option<LastTurn>,
    /// N4 — the keys the user dismissed on this track.
    pub dismissed: BTreeSet<String>,
}

/// The open items, newest first then by key. An ask is open until it is answered (the rows hold
/// only unanswered asks), the user messages the Planner after it (`at_ms > U`), or the user
/// dismisses it; the last two close the notification only, they do not answer. Planner down is
/// open while the newest non-interrupted turn is `failed` and its key is not dismissed. No closed
/// filter: a closed track keeps what is still addressed to the user.
pub fn notifications(track_id: &str, rows: &NotificationRows) -> Vec<ActivityItem> {
    let mut items = Vec::new();

    for ask in &rows.asks {
        let key = format!("{ASK_KEY}{}", ask.ask_id);
        let replied = rows.user_sent_at.is_some_and(|at| at >= ask.at_ms);
        if replied || rows.dismissed.contains(&key) {
            continue;
        }
        items.push(ActivityItem::Ask {
            key,
            text: ask
                .questions
                .iter()
                .map(|q| q.title.as_str())
                .collect::<Vec<_>>()
                .join(" / "),
            at_ms: ask.at_ms,
            ask_id: ask.ask_id,
            questions: ask.questions.clone(),
        });
    }
    if let Some(turn) = &rows.last_turn
        && turn.status.as_deref() == Some("failed")
    {
        let key = format!("{PLANNER_DOWN_KEY}{}", turn.row_id);
        match turn.error_message.as_deref().map(readable_error_text) {
            _ if rows.dismissed.contains(&key) => {}
            Some(text) => items.push(ActivityItem::PlannerDown {
                key,
                text,
                at_ms: turn.at_ms,
            }),
            None => tracing::warn!(
                track_id = %track_id,
                key = %key,
                "track_activity: notification evidence has no text; item dropped"
            ),
        }
    }

    items.sort_by(|a, b| b.at_ms().cmp(&a.at_ms()).then_with(|| a.key().cmp(b.key())));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(at_ms: i64) -> NotificationRows {
        NotificationRows {
            asks: vec![OpenAsk {
                ask_id: 7,
                at_ms,
                questions: vec![
                    AskQuestion {
                        title: "Merge PR #7?".into(),
                        options: vec!["Merge".into(), "Hold".into()],
                    },
                    AskQuestion {
                        title: "Which branch?".into(),
                        options: Vec::new(),
                    },
                ],
            }],
            ..NotificationRows::default()
        }
    }

    #[test]
    fn an_unanswered_ask_is_one_item_with_its_questions() {
        let rows = open(100);
        assert_eq!(
            notifications("t", &rows),
            vec![ActivityItem::Ask {
                key: "ask:7".into(),
                text: "Merge PR #7? / Which branch?".into(),
                at_ms: 100,
                ask_id: 7,
                questions: rows.asks[0].questions.clone(),
            }]
        );
        assert!(is_item_key("ask:7"));
        assert!(
            !is_item_key("ask:ratify:7"),
            "a v2 key is no longer an item key"
        );
        assert!(!is_item_key("ask:notify:7"));
    }

    #[test]
    fn a_later_user_message_or_a_dismissal_closes_the_ask() {
        let replied = NotificationRows {
            user_sent_at: Some(101),
            ..open(100)
        };
        assert!(notifications("t", &replied).is_empty());
        let earlier_reply = NotificationRows {
            user_sent_at: Some(99),
            ..open(100)
        };
        assert_eq!(notifications("t", &earlier_reply).len(), 1);
        let dismissed = NotificationRows {
            dismissed: BTreeSet::from(["ask:7".to_string()]),
            ..open(100)
        };
        assert!(notifications("t", &dismissed).is_empty());
    }

    /// The item is tagged by `source`: a `planner_down` item has no ask fields at all.
    #[test]
    fn items_serialize_tagged_by_source() {
        let down = ActivityItem::PlannerDown {
            key: "planner_down:3".into(),
            text: "boom".into(),
            at_ms: 1,
        };
        assert_eq!(
            serde_json::to_value(&down).unwrap(),
            serde_json::json!({
                "source": "planner_down", "key": "planner_down:3", "text": "boom", "at_ms": 1,
            })
        );
        let ask = notifications("t", &open(100)).remove(0);
        let wire = serde_json::to_value(&ask).unwrap();
        assert_eq!(wire["source"], "ask");
        assert_eq!(wire["ask_id"], 7);
        assert_eq!(wire["questions"][0]["options"][1], "Hold");
    }
}
