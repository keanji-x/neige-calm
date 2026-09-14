//! #1667 A2 — the `ReportEdited` fold keeps the FIRST entry's
//! `body_before` and the NEWEST entry's `body` / `body_sha256` / `author`,
//! so the diff the planner reads spans every save in the fold.

use std::collections::VecDeque;

use calm_types::event::EditAuthor;
use calm_types::ids::TrackId;

use super::{FoldOutcome, QueueEntry, try_fold_tail};
use crate::harness::observation::Observation;

fn edit(author: EditAuthor, before: Option<&str>, after: &str) -> QueueEntry {
    QueueEntry::system(
        Observation::ReportEdited {
            track_id: TrackId::from("track-1"),
            body_sha256: format!("sha:{after}"),
            body: after.to_string(),
            author: Some(author),
            body_before: before.map(str::to_string),
        },
        None,
    )
    .expect("system entry")
}

fn folded_report_edit(queue: &VecDeque<QueueEntry>) -> Observation {
    assert_eq!(queue.len(), 1, "three saves fold into one entry");
    queue[0].observation()
}

#[test]
fn three_report_edits_fold_to_first_before_and_newest_after() {
    let mut queue = VecDeque::from(vec![edit(EditAuthor::User, Some("v0"), "v1")]);
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit(EditAuthor::Plugin, Some("v1"), "v2"),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit(EditAuthor::Assistant, Some("v2"), "v3"),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    let Observation::ReportEdited {
        body_before,
        body,
        body_sha256,
        author,
        ..
    } = folded_report_edit(&queue)
    else {
        panic!("the survivor is still a ReportEdited");
    };
    assert_eq!(
        body_before.as_deref(),
        Some("v0"),
        "the FIRST save's before"
    );
    assert_eq!(body, "v3", "the NEWEST save's after");
    assert_eq!(body_sha256, "sha:v3");
    assert_eq!(author, Some(EditAuthor::Assistant), "the NEWEST author");
}

/// A pre-#1667 first entry has no `body_before`; the fold adopts the
/// incoming one so the diff starts as early as the queue can know.
#[test]
fn a_legacy_first_entry_adopts_the_incoming_body_before() {
    let mut queue = VecDeque::from(vec![edit(EditAuthor::User, None, "v1")]);
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit(EditAuthor::User, Some("v1"), "v2"),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    let Observation::ReportEdited { body_before, .. } = folded_report_edit(&queue) else {
        panic!("the survivor is still a ReportEdited");
    };
    assert_eq!(body_before.as_deref(), Some("v1"));
}
