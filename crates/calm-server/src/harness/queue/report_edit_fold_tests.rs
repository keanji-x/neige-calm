//! The `ReportEdited` fold keeps the FIRST entry's `body_before` and the NEWEST entry's body,
//! attribution and refs; only a CONTIGUOUS save folds.

use std::collections::VecDeque;

use calm_types::event::EditAuthor;
use calm_types::ids::TrackId;
use calm_types::report_edit_diff::ReportBlockRef;

use super::{FoldOutcome, QueueEntry, try_fold_tail};
use crate::harness::observation::Observation;

fn edit(author: EditAuthor, before: Option<&str>, after: &str) -> QueueEntry {
    edit_with_refs(author, before, after, None)
}

fn edit_with_refs(
    author: EditAuthor,
    before: Option<&str>,
    after: &str,
    refs: Option<(u64, &str, u32)>,
) -> QueueEntry {
    QueueEntry::system(
        Observation::ReportEdited {
            track_id: TrackId::from("track-1"),
            body_sha256: format!("sha:{after}"),
            body: after.to_string(),
            author: Some(author),
            body_before: before.map(str::to_string),
            doc_rev_after: refs.map(|(doc_rev, _, _)| doc_rev),
            blocks_after: refs.map(|(_, id, rev)| vec![ReportBlockRef { id: id.into(), rev }]),
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

/// Adopting the incoming `Some(v1)` would start the diff at v1 and drop the v0 -> v1 edit
/// from everywhere the planner can see it.
#[test]
fn a_legacy_first_entry_keeps_body_before_none() {
    let mut queue = VecDeque::from(vec![edit(EditAuthor::User, None, "v1")]);
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit(EditAuthor::User, Some("v1"), "v2"),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    let Observation::ReportEdited {
        body_before, body, ..
    } = folded_report_edit(&queue)
    else {
        panic!("the survivor is still a ReportEdited");
    };
    assert_eq!(body_before, None, "the legacy first entry stays legacy");
    assert_eq!(body, "v2", "the NEWEST save's after");
}

/// Something else wrote the report in between; a fold across it would attribute that write's
/// lines to the user.
#[test]
fn a_non_contiguous_report_edit_does_not_fold() {
    let mut queue = VecDeque::from(vec![edit(EditAuthor::User, Some("v0"), "v1")]);
    let incoming = edit(EditAuthor::User, Some("v1-planner"), "v2");
    assert_eq!(
        try_fold_tail(&mut queue, &incoming, 10_000),
        FoldOutcome::NotFolded
    );
    assert_eq!(queue.len(), 1, "a refused fold leaves the tail untouched");
    let Observation::ReportEdited {
        body_before, body, ..
    } = queue[0].observation()
    else {
        panic!("the tail is still a ReportEdited");
    };
    assert_eq!(body_before.as_deref(), Some("v0"));
    assert_eq!(body, "v1");
    // The same save, contiguous, folds.
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit(EditAuthor::User, Some("v1"), "v2"),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    assert_eq!(queue.len(), 1);
}

/// `doc_rev_after` / `blocks_after` describe `body`, so the fold takes the NEWEST entry's pair.
#[test]
fn fold_takes_the_newest_doc_rev_and_block_refs() {
    let mut queue = VecDeque::from(vec![edit_with_refs(
        EditAuthor::User,
        Some("v0"),
        "v1",
        Some((7, "b_0001", 2)),
    )]);
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit_with_refs(EditAuthor::User, Some("v1"), "v2", Some((8, "b_0001", 3))),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    let Observation::ReportEdited {
        doc_rev_after,
        blocks_after,
        ..
    } = folded_report_edit(&queue)
    else {
        panic!("the survivor is still a ReportEdited");
    };
    assert_eq!(doc_rev_after, Some(8), "the NEWEST save's docRev");
    assert_eq!(
        blocks_after,
        Some(vec![ReportBlockRef {
            id: "b_0001".into(),
            rev: 3,
        }]),
        "the NEWEST save's block refs"
    );

    // A newest save whose refs did not align clears both: an older pair
    // would describe a body the diff no longer shows.
    assert!(matches!(
        try_fold_tail(
            &mut queue,
            &edit_with_refs(EditAuthor::User, Some("v2"), "v3", None),
            10_000
        ),
        FoldOutcome::Folded { .. }
    ));
    let Observation::ReportEdited {
        doc_rev_after,
        blocks_after,
        body,
        ..
    } = folded_report_edit(&queue)
    else {
        panic!("the survivor is still a ReportEdited");
    };
    assert_eq!(body, "v3");
    assert_eq!(doc_rev_after, None);
    assert_eq!(blocks_after, None);
}
