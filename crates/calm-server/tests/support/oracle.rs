use std::collections::HashMap;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::ids::ActorId;
use serde_json::Value;

use super::event_queries::{EventRow, event_rows};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubjectKey {
    pub phase: String,
    pub slice_id: String,
    pub pr_number: Option<u64>,
}

impl SubjectKey {
    pub fn from_subject_payload(subject: &Value) -> Self {
        Self {
            phase: subject["phase"]
                .as_str()
                .expect("subject.phase")
                .to_string(),
            slice_id: subject["slice_id"]
                .as_str()
                .expect("subject.slice_id")
                .to_string(),
            pr_number: subject.get("pr_number").and_then(Value::as_u64),
        }
    }
}

pub fn review_subject_key(row: &EventRow) -> SubjectKey {
    SubjectKey::from_subject_payload(&row.payload["subject"])
}

pub fn review_round_n(row: &EventRow) -> u32 {
    row.payload["n"].as_u64().expect("review n") as u32
}

pub fn review_round_cap(row: &EventRow) -> u32 {
    row.payload["cap"].as_u64().expect("review cap") as u32
}

pub fn review_round_converged(row: &EventRow) -> bool {
    row.payload["converged"]
        .as_bool()
        .expect("review converged")
}

pub fn merge_matches_subject(row: &EventRow, key: &SubjectKey) -> bool {
    SubjectKey::from_subject_payload(&row.payload["subject"]) == *key
}

pub fn row_head_sha(row: &EventRow) -> Option<String> {
    row.payload
        .get("head_sha")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub async fn assert_subject_keyed_cap_enforcement(repo: &SqlxRepo, wave_id: &str) {
    let rounds = event_rows(repo, "review.round").await;
    let merges = event_rows(repo, "forge.pr.merged").await;
    let issue_closed = event_rows(repo, "forge.issue.closed").await;
    let lifecycle = event_rows(repo, "wave.lifecycle_changed").await;
    let ratify_resolved = event_rows(repo, "ratify.resolved").await;

    let mut max_round_by_subject: HashMap<SubjectKey, EventRow> = HashMap::new();
    for round in &rounds {
        let key = review_subject_key(round);
        let replace = max_round_by_subject
            .get(&key)
            .map(|existing| review_round_n(round) > review_round_n(existing))
            .unwrap_or(true);
        if replace {
            max_round_by_subject.insert(key, round.clone());
        }
    }

    for (key, max_round) in max_round_by_subject {
        if review_round_converged(&max_round) {
            if key.pr_number.is_some() {
                let expected = row_head_sha(&max_round).expect("converged PR review head_sha");
                for merge in merges.iter().filter(|row| merge_matches_subject(row, &key)) {
                    assert_eq!(
                        row_head_sha(merge).as_deref(),
                        Some(expected.as_str()),
                        "merge head must match latest max-n converged review for {key:?}"
                    );
                }
            }
            continue;
        }

        let later_grant = ratify_resolved.iter().find(|row| {
            row.id > max_round.id
                && row.scope_wave.as_deref() == Some(wave_id)
                && row.payload["decision"] == "grant"
        });
        let later_converged = later_grant.and_then(|grant| {
            rounds
                .iter()
                .filter(|row| {
                    row.id > grant.id
                        && review_subject_key(row) == key
                        && review_round_converged(row)
                })
                .max_by_key(|row| review_round_n(row))
        });

        if let Some(converged) = later_converged {
            let expected = row_head_sha(converged).expect("later converged review head_sha");
            for merge in merges.iter().filter(|row| merge_matches_subject(row, &key)) {
                assert_eq!(
                    row_head_sha(merge).as_deref(),
                    Some(expected.as_str()),
                    "post-ratify merge head must match intervening converged review for {key:?}"
                );
            }
            continue;
        }

        assert!(
            !merges
                .iter()
                .any(|row| row.id > max_round.id && merge_matches_subject(row, &key)),
            "unconverged max-n subject {key:?} must not merge later"
        );
        assert!(
            !issue_closed.iter().any(|row| row.id > max_round.id),
            "unconverged max-n subject {key:?} must not close an issue later"
        );
        assert!(
            !lifecycle.iter().any(|row| {
                row.id > max_round.id
                    && row.scope_wave.as_deref() == Some(wave_id)
                    && row.payload["to"] == "done"
            }),
            "unconverged max-n subject {key:?} must not reach done later"
        );
    }
}

/// Direct executable form of INV-CAP-EXT (#888 design §3.3/§7c′), validating
/// the wave's `review.round` rows however they were written: per subject, in
/// event-row-id order, every adjacent pair `(prev, next)` must satisfy
/// `n(next) == n(prev) + 1` and either `cap(next) == cap(prev)` (in-window) or
/// all of `cap(next) == cap(prev) + 2` AND `n(prev) == cap(prev)` (prev
/// exhausted) AND an intervening `ratify.resolved { grant }` witness row with
/// `prev.id < g.id < next.id` authored by `ActorId::User` (role_gate 2.9 —
/// the User-actor condition keeps the audit claim honest even against a
/// malformed/test-seeded non-User grant). Grant non-reuse follows
/// observationally: two extensions of one subject bracket witnesses in
/// disjoint id intervals.
///
/// Returns per-subject extension counts so callers can pin exact totals.
pub async fn assert_cap_extension_history(
    repo: &SqlxRepo,
    wave_id: &str,
) -> HashMap<SubjectKey, usize> {
    let rounds: Vec<EventRow> = event_rows(repo, "review.round")
        .await
        .into_iter()
        .filter(|row| row.scope_wave.as_deref() == Some(wave_id))
        .collect();

    let resolved_rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, actor, scope_wave, payload FROM events \
         WHERE kind = 'ratify.resolved' ORDER BY id ASC",
    )
    .fetch_all(repo.pool())
    .await
    .expect("ratify.resolved rows");
    let user_grant_ids: Vec<i64> = resolved_rows
        .into_iter()
        .filter_map(|(id, actor, scope_wave, payload)| {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            (scope_wave.as_deref() == Some(wave_id)
                && payload["decision"] == "grant"
                && actor == ActorId::User)
                .then_some(id)
        })
        .collect();

    let mut by_subject: HashMap<SubjectKey, Vec<EventRow>> = HashMap::new();
    for round in rounds {
        by_subject
            .entry(review_subject_key(&round))
            .or_default()
            .push(round);
    }

    let mut extensions: HashMap<SubjectKey, usize> = HashMap::new();
    for (key, mut group) in by_subject {
        group.sort_by_key(|row| row.id);
        let mut count = 0usize;
        for pair in group.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            let (prev_n, prev_cap) = (review_round_n(prev), review_round_cap(prev));
            let (next_n, next_cap) = (review_round_n(next), review_round_cap(next));
            // checked_add: on adversarial history at the u32 boundary the
            // successor does not exist — fail the assertion cleanly (None !=
            // Some) instead of panicking on arithmetic overflow.
            assert_eq!(
                Some(next_n),
                prev_n.checked_add(1),
                "INV-CAP-EXT: n must rise by exactly 1 for {key:?}: n={prev_n} (id {}) -> n={next_n} (id {})",
                prev.id,
                next.id
            );
            if next_cap == prev_cap {
                continue;
            }
            assert_eq!(
                Some(next_cap),
                prev_cap.checked_add(2),
                "INV-CAP-EXT: a cap change must be exactly +2 for {key:?}: cap={prev_cap} (id {}) -> cap={next_cap} (id {})",
                prev.id,
                next.id
            );
            assert_eq!(
                prev_n, prev_cap,
                "INV-CAP-EXT: an extension requires the previous round to exhaust its window \
                 for {key:?}: n={prev_n}, cap={prev_cap} (id {})",
                prev.id
            );
            assert!(
                user_grant_ids
                    .iter()
                    .any(|grant_id| prev.id < *grant_id && *grant_id < next.id),
                "INV-CAP-EXT: an extension requires a User-authored ratify.resolved{{grant}} \
                 witness strictly between id {} and id {} for {key:?}; user grant ids: \
                 {user_grant_ids:?}",
                prev.id,
                next.id
            );
            count += 1;
        }
        extensions.insert(key, count);
    }
    extensions
}

/// First (smallest) event id among rows matching `pred`, if any.
pub fn first_matching_id(rows: &[EventRow], pred: impl Fn(&EventRow) -> bool) -> Option<i64> {
    rows.iter().filter(|r| pred(r)).map(|r| r.id).min()
}

/// head_sha of the latest-n review.round for `subject`, but only if that
/// latest round is converged (else None). Implements the 6a "converged latest".
pub fn latest_converged_head_sha(rounds: &[EventRow], subject: &SubjectKey) -> Option<String> {
    rounds
        .iter()
        .filter(|r| &review_subject_key(r) == subject)
        .max_by_key(|r| review_round_n(r))
        .filter(|r| review_round_converged(r))
        .and_then(row_head_sha)
}

/// Does any forge.pr.merged row for `subject` carry `head_sha`?
pub fn any_merge_for_subject_with_head(
    merges: &[EventRow],
    subject: &SubjectKey,
    head_sha: &str,
) -> bool {
    merges
        .iter()
        .any(|r| merge_matches_subject(r, subject) && row_head_sha(r).as_deref() == Some(head_sha))
}

/// Pure core: Ok(()) iff the subject's latest-n review.round is converged AND a
/// matching merge carries its head_sha; otherwise Err with a reason.
pub fn converged_subject_merge_check(
    rounds: &[EventRow],
    merges: &[EventRow],
    subject: &SubjectKey,
) -> Result<(), String> {
    let head = latest_converged_head_sha(rounds, subject)
        .ok_or_else(|| format!("subject {subject:?} has no latest-n converged review.round"))?;
    if any_merge_for_subject_with_head(merges, subject, &head) {
        Ok(())
    } else {
        Err(format!(
            "converged subject {subject:?} (head {head}) has no matching forge.pr.merged"
        ))
    }
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

/// A happens-before edge: first row matching `before` (of `before_kind`) must
/// precede first row matching `after` (of `after_kind`). Vacuously satisfied if
/// either side is absent - presence is enforced separately via
/// `assert_event_skeleton_superset`. The closures cover payload predicates such
/// as `forge.pr.checks` `conclusion == "success"` (edge-4).
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

/// Pure core: the offending (before_id, after_id) pair if the first matching
/// `before` row does NOT precede the first matching `after` row. None if either
/// side is absent (vacuously satisfied) or if ordered correctly.
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

/// 6a existence assertion the lifted cap helper lacks (it passes vacuously with
/// zero merges). For a converged impl `subject`, at least one `forge.pr.merged`
/// with the latest converged round's `head_sha` MUST exist.
pub async fn assert_converged_subject_has_merge(repo: &SqlxRepo, subject: &SubjectKey) {
    let rounds = event_rows(repo, "review.round").await;
    let merges = event_rows(repo, "forge.pr.merged").await;
    converged_subject_merge_check(&rounds, &merges, subject).unwrap_or_else(|e| panic!("{e}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: i64, payload: serde_json::Value) -> EventRow {
        EventRow {
            id,
            scope_kind: "wave".to_string(),
            scope_wave: Some("w1".to_string()),
            scope_card: None,
            payload,
        }
    }

    fn review_row(
        id: i64,
        phase: &str,
        slice: &str,
        pr: Option<u64>,
        n: u64,
        converged: bool,
        head: &str,
    ) -> EventRow {
        let mut subject = json!({"phase": phase, "slice_id": slice});
        if let Some(pr) = pr {
            subject["pr_number"] = json!(pr);
        }
        row(
            id,
            json!({"subject": subject, "n": n, "converged": converged, "head_sha": head}),
        )
    }

    fn merge_row(id: i64, phase: &str, slice: &str, pr: u64, head: &str) -> EventRow {
        row(
            id,
            json!({"subject": {"phase": phase, "slice_id": slice, "pr_number": pr}, "head_sha": head}),
        )
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
    fn latest_converged_head_sha_uses_max_n_and_requires_converged() {
        let subj = SubjectKey {
            phase: "impl".into(),
            slice_id: "s".into(),
            pr_number: Some(7),
        };
        // latest (n=3) converged -> Some(head3)
        let rounds = vec![
            review_row(1, "impl", "s", Some(7), 1, false, "h1"),
            review_row(2, "impl", "s", Some(7), 3, true, "h3"),
            review_row(3, "impl", "s", Some(7), 2, true, "h2"),
        ];
        assert_eq!(
            latest_converged_head_sha(&rounds, &subj),
            Some("h3".to_string())
        );
        // latest (n=3) NOT converged -> None even though an earlier round converged
        let rounds2 = vec![
            review_row(1, "impl", "s", Some(7), 2, true, "h2"),
            review_row(2, "impl", "s", Some(7), 3, false, "h3"),
        ];
        assert_eq!(latest_converged_head_sha(&rounds2, &subj), None);
    }

    #[test]
    fn any_merge_for_subject_with_head_matches_subject_and_head() {
        let subj = SubjectKey {
            phase: "impl".into(),
            slice_id: "s".into(),
            pr_number: Some(7),
        };
        let other = SubjectKey {
            phase: "impl".into(),
            slice_id: "other".into(),
            pr_number: Some(9),
        };
        let merges = vec![
            merge_row(1, "impl", "s", 7, "h3"),
            merge_row(2, "impl", "other", 9, "hx"),
            merge_row(3, "impl", "other", 9, "h3"),
        ];
        assert!(any_merge_for_subject_with_head(&merges, &subj, "h3"));
        assert!(!any_merge_for_subject_with_head(&merges, &subj, "hnope"));
        // "hx" exists in the set but only for a DIFFERENT subject -> a subject-ignoring
        // implementation would wrongly return true here.
        assert!(!any_merge_for_subject_with_head(&merges, &subj, "hx"));
        assert!(any_merge_for_subject_with_head(&merges, &other, "h3"));
        let other_h3_matches = merges
            .iter()
            .filter(|row| {
                merge_matches_subject(row, &other) && row_head_sha(row).as_deref() == Some("h3")
            })
            .count();
        assert_eq!(other_h3_matches, 1);
    }

    #[test]
    fn skeleton_superset_missing_reports_absent_kind() {
        let required = [
            RequiredEvent::new("review.round", review_round_converged),
            RequiredEvent::any("forge.pr.merged"),
        ];
        let checked = vec![
            (
                &required[0],
                vec![review_row(1, "impl", "s", Some(7), 1, true, "h1")],
            ),
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

    #[test]
    fn converged_subject_merge_check_requires_latest_converged_matching_merge() {
        let subj = SubjectKey {
            phase: "impl".into(),
            slice_id: "s".into(),
            pr_number: Some(7),
        };

        let rounds = vec![review_row(1, "impl", "s", Some(7), 1, true, "h1")];
        let merges = vec![merge_row(2, "impl", "s", 7, "h1")];
        assert_eq!(
            converged_subject_merge_check(&rounds, &merges, &subj),
            Ok(())
        );

        let missing_merge = converged_subject_merge_check(&rounds, &[], &subj).unwrap_err();
        assert!(missing_merge.contains("has no matching forge.pr.merged"));

        let not_latest_converged = vec![
            review_row(1, "impl", "s", Some(7), 1, true, "h1"),
            review_row(2, "impl", "s", Some(7), 2, false, "h2"),
        ];
        let err = converged_subject_merge_check(&not_latest_converged, &merges, &subj).unwrap_err();
        assert!(err.contains("has no latest-n converged review.round"));
    }
}
