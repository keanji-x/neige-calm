//! The two notification sources of a track (#1829): an ask (a pending `calm.ratify.request`, or a
//! `calm.user.notify` call) and planner down (the Planner's newest finished turn failed). A pure
//! function of the rows `sql::notification_rows` read; nothing else is an item.

use std::collections::BTreeSet;

use calm_truth::readable_error_text::readable_error_text;
use serde::{Deserialize, Serialize};

/// The three key shapes: a source prefix, then the evidence row's id (`events.id` for a ratify
/// request, the transcript row id for a notify call or a failed turn).
const ASK_RATIFY_KEY: &str = "ask:ratify:";
const ASK_NOTIFY_KEY: &str = "ask:notify:";
const PLANNER_DOWN_KEY: &str = "planner_down:";

/// Whether `key` has one of the three item key shapes (`<prefix><decimal row id>`). The Dismiss
/// route admits only these; whether the item is still open is not asked.
pub fn is_item_key(key: &str) -> bool {
    [ASK_RATIFY_KEY, ASK_NOTIFY_KEY, PLANNER_DOWN_KEY]
        .iter()
        .filter_map(|prefix| key.strip_prefix(prefix))
        .any(|id| id.bytes().all(|b| b.is_ascii_digit()) && id.parse::<i64>().is_ok())
}

/// What an item is: `ask` folds to `attention = input`, `planner_down` to `attention = failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSource {
    Ask,
    PlannerDown,
}

/// One thing addressed to the user and not yet handled. `key` carries the evidence row's id, so the
/// same source happening again is a new key; `text` is the kernel's words for it, shown verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityItem {
    pub source: NotificationSource,
    pub key: String,
    pub text: String,
    pub at_ms: i64,
}

/// N1 — the track's newest `ratify.requested`, when no `ratify.*` event follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRatify {
    pub event_id: i64,
    pub at_ms: i64,
    /// `payload.reason`; the tool refuses a request without one.
    pub reason: Option<String>,
}

/// N3, notify arm — one successful `calm.user.notify` call of the Planner card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyRow {
    pub row_id: i64,
    pub at_ms: i64,
    /// `$.item.arguments.text`; the tool refuses a call without it.
    pub text: Option<String>,
}

/// N3, turn arm — the Planner card's newest `turn/completed` row that is not `interrupted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTurn {
    pub row_id: i64,
    pub at_ms: i64,
    pub status: Option<String>,
    /// `$.error.message`.
    pub error_message: Option<String>,
}

/// Everything the two sources read. A track without a Planner card has no notify rows, no last
/// turn and no U.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationRows {
    pub pending_ratify: Option<PendingRatify>,
    /// U — the newest user message to the Planner card.
    pub user_sent_at: Option<i64>,
    pub notifies: Vec<NotifyRow>,
    pub last_turn: Option<LastTurn>,
    /// N4 — the keys the user dismissed on this track.
    pub dismissed: BTreeSet<String>,
}

/// One item, or none when the user dismissed its key, or when its required text decoded to NULL:
/// only that item is dropped, with a warn naming its key; the other items and the overlay are
/// written as usual.
fn item(
    track_id: &str,
    dismissed: &BTreeSet<String>,
    source: NotificationSource,
    key: String,
    text: Option<&str>,
    at_ms: i64,
) -> Option<ActivityItem> {
    if dismissed.contains(&key) {
        return None;
    }
    let Some(text) = text else {
        tracing::warn!(
            track_id = %track_id,
            key = %key,
            "track_activity: notification evidence has no text; item dropped"
        );
        return None;
    };
    Some(ActivityItem {
        source,
        key,
        text: text.to_string(),
        at_ms,
    })
}

/// The open items, newest first then by key. An ask is open while `at_ms > U`; planner down is
/// open while the newest non-interrupted turn is `failed`; a dismissed key is never an item. No
/// closed filter: a closed track keeps what is still addressed to the user.
pub fn notifications(track_id: &str, rows: &NotificationRows) -> Vec<ActivityItem> {
    let open = |at_ms: i64| rows.user_sent_at.is_none_or(|answered| at_ms > answered);
    let mut items = Vec::new();

    if let Some(ratify) = &rows.pending_ratify
        && open(ratify.at_ms)
    {
        items.extend(item(
            track_id,
            &rows.dismissed,
            NotificationSource::Ask,
            format!("{ASK_RATIFY_KEY}{}", ratify.event_id),
            ratify.reason.as_deref(),
            ratify.at_ms,
        ));
    }
    for notify in rows.notifies.iter().filter(|n| open(n.at_ms)) {
        items.extend(item(
            track_id,
            &rows.dismissed,
            NotificationSource::Ask,
            format!("{ASK_NOTIFY_KEY}{}", notify.row_id),
            notify.text.as_deref().map(str::trim),
            notify.at_ms,
        ));
    }
    if let Some(turn) = &rows.last_turn
        && turn.status.as_deref() == Some("failed")
    {
        let text = turn.error_message.as_deref().map(readable_error_text);
        items.extend(item(
            track_id,
            &rows.dismissed,
            NotificationSource::PlannerDown,
            format!("{PLANNER_DOWN_KEY}{}", turn.row_id),
            text.as_deref(),
            turn.at_ms,
        ));
    }

    items.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.key.cmp(&b.key)));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(at_ms: i64) -> NotificationRows {
        NotificationRows {
            pending_ratify: Some(PendingRatify {
                event_id: 7,
                at_ms,
                reason: Some("merge the release branch?".into()),
            }),
            ..NotificationRows::default()
        }
    }

    #[test]
    fn pending_ratify_is_an_open_ask_until_answered() {
        let items = notifications("t", &pending(100));
        assert_eq!(
            items,
            vec![ActivityItem {
                source: NotificationSource::Ask,
                key: "ask:ratify:7".into(),
                text: "merge the release branch?".into(),
                at_ms: 100,
            }]
        );
        assert!(is_item_key("ask:ratify:7"));

        let answered = NotificationRows {
            user_sent_at: Some(101),
            ..pending(100)
        };
        assert!(
            notifications("t", &answered).is_empty(),
            "a user reply after the request closes the ask"
        );
        let earlier_reply = NotificationRows {
            user_sent_at: Some(99),
            ..pending(100)
        };
        assert_eq!(notifications("t", &earlier_reply).len(), 1);
    }
}
