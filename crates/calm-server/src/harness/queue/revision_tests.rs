use super::*;

fn at_limit() -> QueueEntry {
    let mut entry = QueueEntry::user_message("keep this input".into(), None, Vec::new());
    let QueueEntry::User { rev, .. } = &mut entry else {
        unreachable!()
    };
    *rev = u32::MAX;
    entry
}

#[test]
fn versioned_input_exhaustion_refuses_edit_without_changing_the_entry() {
    let entry = at_limit();
    let id = entry.id().unwrap().clone();
    let mut queue = VecDeque::from([entry]);
    let before = queue.clone();
    assert!(
        apply_mutation(
            &mut queue,
            &QueueMutation::Edit {
                entry_id: id,
                if_entry_rev: u32::MAX,
                text: "must not replace the body".into(),
            }
        )
        .is_err()
    );
    assert_eq!(queue, before);
}

#[test]
fn versioned_input_exhaustion_refuses_steer_without_taking_the_entry() {
    let entry = at_limit();
    let id = entry.id().unwrap().clone();
    let mut queue = VecDeque::from([entry]);
    let before = queue.clone();
    assert!(
        apply_mutation(
            &mut queue,
            &QueueMutation::Steer {
                entry_id: id,
                if_entry_rev: u32::MAX,
            }
        )
        .is_err()
    );
    assert_eq!(queue, before);
}

#[test]
fn versioned_input_exhaustion_declines_fold_without_changing_the_body() {
    let mut queue = VecDeque::from([at_limit()]);
    let before = queue.clone();
    let incoming = QueueEntry::user_message("must remain separate".into(), None, Vec::new());
    assert_eq!(
        try_fold_tail(&mut queue, &incoming, usize::MAX),
        FoldOutcome::NotFolded
    );
    assert_eq!(queue, before);
}

#[test]
fn versioned_input_exhaustion_keeps_delete_and_retained_restore_safe() {
    let mut retained = at_limit();
    let before = retained.clone();
    assert!(!retained.bump_rev_for_restore());
    assert_eq!(retained, before, "retained input cannot panic or disappear");
    let id = retained.id().unwrap().clone();
    let mut queue = VecDeque::from([retained]);
    apply_mutation(
        &mut queue,
        &QueueMutation::Delete {
            entry_id: id,
            if_entry_rev: u32::MAX,
        },
    )
    .unwrap();
    assert!(queue.is_empty());
}
