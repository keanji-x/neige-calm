use calm_server::db::sqlite::SqlxRepo;
use serde_json::Value;

use super::event_queries::{EventRow, event_rows};

pub fn row_head_sha(row: &EventRow) -> Option<String> {
    row.payload
        .get("head_sha")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// First (smallest) event id among rows matching `pred`, if any.
pub fn first_matching_id(rows: &[EventRow], pred: impl Fn(&EventRow) -> bool) -> Option<i64> {
    rows.iter().filter(|r| pred(r)).map(|r| r.id).min()
}

/// A required event the log must contain >=1 of: a `kind` + a predicate over the row.
pub struct RequiredEvent {
    pub kind: &'static str,
    pub matches: Box<dyn Fn(&EventRow) -> bool + Send + Sync>,
}

impl RequiredEvent {
    pub fn new(
        kind: &'static str,
        matches: impl Fn(&EventRow) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            matches: Box::new(matches),
        }
    }

    /// Match any row of the kind (presence-only).
    pub fn any(kind: &'static str) -> Self {
        Self::new(kind, |_| true)
    }
}

/// Pure core: the kinds among `checked` that have NO row satisfying their predicate.
pub fn skeleton_superset_missing(checked: &[(&RequiredEvent, Vec<EventRow>)]) -> Vec<&'static str> {
    checked
        .iter()
        .filter(|(req, rows)| !rows.iter().any(|r| (req.matches)(r)))
        .map(|(req, _)| req.kind)
        .collect()
}

/// Assert every required (kind, predicate) appears at least once in the event log.
/// Superset semantics: extra events are tolerated; only presence of each required is checked.
pub async fn assert_event_skeleton_superset(repo: &SqlxRepo, required: &[RequiredEvent]) {
    let mut checked: Vec<(&RequiredEvent, Vec<EventRow>)> = Vec::new();
    for req in required {
        let rows = event_rows(repo, req.kind).await;
        checked.push((req, rows));
    }
    let missing = skeleton_superset_missing(&checked);
    assert!(
        missing.is_empty(),
        "event skeleton missing required kinds: {missing:?}"
    );
}

/// A happens-before edge: first row matching `before` must precede first row matching `after`; vacuously satisfied if either side is absent.
pub struct OrderingEdge {
    pub before_kind: &'static str,
    pub before: Box<dyn Fn(&EventRow) -> bool + Send + Sync>,
    pub after_kind: &'static str,
    pub after: Box<dyn Fn(&EventRow) -> bool + Send + Sync>,
}

impl OrderingEdge {
    pub fn new(
        before_kind: &'static str,
        before: impl Fn(&EventRow) -> bool + Send + Sync + 'static,
        after_kind: &'static str,
        after: impl Fn(&EventRow) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            before_kind,
            before: Box::new(before),
            after_kind,
            after: Box::new(after),
        }
    }
}

/// The offending (before_id, after_id) pair if the first matching `before` row does NOT precede the first matching `after`; None if either side is absent.
pub fn ordering_violation(
    before_rows: &[EventRow],
    before: &dyn Fn(&EventRow) -> bool,
    after_rows: &[EventRow],
    after: &dyn Fn(&EventRow) -> bool,
) -> Option<(i64, i64)> {
    match (
        first_matching_id(before_rows, before),
        first_matching_id(after_rows, after),
    ) {
        (Some(b), Some(a)) if b >= a => Some((b, a)),
        _ => None,
    }
}

/// Assert each ordering edge: first matching `before` id < first matching `after` id.
pub async fn assert_ordering(repo: &SqlxRepo, edges: &[OrderingEdge]) {
    for edge in edges {
        let before_rows = event_rows(repo, edge.before_kind).await;
        let after_rows = event_rows(repo, edge.after_kind).await;
        if let Some((b, a)) =
            ordering_violation(&before_rows, &edge.before, &after_rows, &edge.after)
        {
            panic!(
                "ordering violated: {} (id {b}) must precede {} (id {a})",
                edge.before_kind, edge.after_kind
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: i64, payload: serde_json::Value) -> EventRow {
        EventRow {
            id,
            scope_kind: "track".to_string(),
            scope_track: Some("w1".to_string()),
            scope_card: None,
            payload,
        }
    }

    #[test]
    fn first_matching_id_picks_min() {
        let rows = vec![
            row(5, json!({"k":1})),
            row(2, json!({"k":1})),
            row(9, json!({"k":2})),
        ];
        assert_eq!(first_matching_id(&rows, |r| r.payload["k"] == 1), Some(2));
        assert_eq!(first_matching_id(&rows, |r| r.payload["k"] == 3), None);
    }

    #[test]
    fn skeleton_superset_missing_reports_absent_kind() {
        let required = [
            RequiredEvent::new("forge.pr.checks", |r| r.payload["conclusion"] == "success"),
            RequiredEvent::any("forge.pr.merged"),
        ];
        let checked = vec![
            (&required[0], vec![row(1, json!({"conclusion": "success"}))]),
            (&required[1], Vec::new()),
        ];
        assert_eq!(skeleton_superset_missing(&checked), vec!["forge.pr.merged"]);
    }

    #[test]
    fn ordering_violation_reports_only_misordered_present_edges() {
        let is_before = |r: &EventRow| r.payload["role"] == "before";
        let is_after = |r: &EventRow| r.payload["role"] == "after";

        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "before"}))],
                &is_before,
                &[row(2, json!({"role": "after"}))],
                &is_after,
            ),
            None
        );
        assert_eq!(
            ordering_violation(
                &[row(5, json!({"role": "before"}))],
                &is_before,
                &[row(3, json!({"role": "after"}))],
                &is_after,
            ),
            Some((5, 3))
        );
        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "before"}))],
                &is_before,
                &[row(2, json!({"role": "other"}))],
                &is_after,
            ),
            None
        );
        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "other"}))],
                &is_before,
                &[row(2, json!({"role": "after"}))],
                &is_after,
            ),
            None
        );
    }
}
