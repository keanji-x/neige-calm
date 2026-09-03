//! Writer-attribution guards for task declaration blocks (#985 PR2-a).

use std::collections::HashMap;

use calm_types::event::EditAuthor;
use calm_types::report_blocks::{KIND_TASK, render_fence};
use calm_types::track_report::ReportBlock;
use serde_json::{Value, json};

use crate::error::CalmError;
use crate::track_report::ReportDocOp;
use crate::track_report_doc::ReportDoc;

fn bad(message: impl Into<String>) -> CalmError {
    CalmError::BadRequest(message.into())
}

fn field<'a>(block: &'a ReportBlock, name: &str) -> Option<&'a Value> {
    block.payload.get(name)
}

fn string_field<'a>(block: &'a ReportBlock, name: &str) -> Option<&'a str> {
    field(block, name).and_then(Value::as_str)
}

fn is_task(block: &ReportBlock) -> bool {
    block.kind == KIND_TASK
}

fn is_tombstone(block: &ReportBlock) -> bool {
    field(block, "tombstone").is_some_and(|value| !value.is_null())
}

/// The **document** name for an author — the value that goes into a task
/// block's `declared_by` / `tombstoned_by`.
///
/// This is the one place the kernel's renamed vocabulary meets the frozen
/// document vocabulary, so the asymmetry is deliberate: the enum variant is
/// `Planner` (#1316 S3 renamed the actor everywhere the kernel owns it) while
/// the string stays `"spec"`, because `declared_by` is written by agents into
/// stored report documents and projected back out of them — migration 0083's
/// header states why those cannot be rewritten. Returning `"planner"` here
/// deadlocks the two guards against each other: this one demands `planner`
/// while `report_blocks::kinds::validate_declared_by` accepts only
/// `"spec" | "user"`, so every new task block is rejected whichever value the
/// agent writes.
fn author_name(author: EditAuthor) -> Option<&'static str> {
    match author {
        EditAuthor::Planner => Some("spec"),
        EditAuthor::User => Some("user"),
        // `None` = "may not author task declaration blocks". Assistant
        // (#1189) is deliberately here rather than aliased onto "planner":
        // half of the §3.2a P2 guard falls out of this line.
        EditAuthor::Kernel | EditAuthor::Plugin | EditAuthor::Assistant => None,
    }
}

/// Turn a user's block-level deletion of a live task into an in-place tombstone.
pub(crate) fn normalize_report_op(
    doc: &ReportDoc,
    op: ReportDocOp,
    author: EditAuthor,
) -> Result<ReportDocOp, CalmError> {
    let ReportDocOp::DeleteBlock { id, if_rev } = &op else {
        return Ok(op);
    };
    if author != EditAuthor::User {
        return Ok(op);
    }
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!(
            "track_report: snapshot for task delete rewrite: {error}"
        ))
    })?;
    let Some(block) = blocks.iter().find(|block| block.id == *id) else {
        return Ok(op);
    };
    if !is_task(block) || is_tombstone(block) {
        return Ok(op);
    }
    let key =
        string_field(block, "key").ok_or_else(|| bad(format!("task block {id} is missing key")))?;
    let declared_by = string_field(block, "declared_by")
        .ok_or_else(|| bad(format!("task block {id} is missing declared_by")))?;
    let payload = json!({
        "key": key,
        "tombstone": { "reason": null },
        "declared_by": declared_by,
        "tombstoned_by": "user"
    });
    Ok(ReportDocOp::UpsertBlock {
        id: Some(id.clone()),
        kind: KIND_TASK.into(),
        content: render_fence(KIND_TASK, &payload),
        if_rev: Some(*if_rev),
        if_doc_rev: None,
        position: None,
    })
}

/// #1189 §3.2a P2 — an assistant's write must leave the task declarations
/// exactly as it found them.
///
/// ## Why this is a separate rule and not "one more author in the table"
///
/// The provenance rules below already stop an assistant from *creating*
/// a task (no attribution name), from touching a **user**-controlled one,
/// and from dropping a live task through a whole-document write. What they
/// leave open is the middle of the range: editing a **planner**-declared live
/// task in place (flipping `ready`, rewriting `goal`) and block-level
/// deleting one. Both feed `task_projection`, i.e. both can dispatch or
/// re-dispatch a worker — which is the entire thing §3.2 says an assistant
/// must not be able to reach. `#1180` deliberately covers only the
/// `declared_by/tombstoned_by == "user"` half, so nothing else covers this.
///
/// ## The criterion is per-block equivalence, not "no task blocks after"
///
/// `write_markdown` realigns block identity through markers/LCS. A
/// prose-only rewrite that happens to carry the task fences through
/// unchanged must succeed — an assistant editing the prose *around* a
/// task list is the ordinary case, not an attack. So the check is:
/// **the task blocks, keyed by id, are the same set with the same
/// content before and after**. Order is not content (a `MoveBlock` never
/// bumps a rev and is allowed for every non-user author already), and
/// `rev` is not compared for the same reason a no-op content write is not
/// a modification. Anything that adds, removes, re-ids, retypes, or
/// rewrites a task block trips it.
fn guard_assistant_leaves_task_blocks_alone(
    before: &[ReportBlock],
    after: &[ReportBlock],
) -> Result<(), CalmError> {
    fn tasks_by_id(blocks: &[ReportBlock]) -> HashMap<&str, (&str, &Value)> {
        blocks
            .iter()
            .filter(|block| is_task(block))
            .map(|block| (block.id.as_str(), (block.kind.as_str(), &block.payload)))
            .collect()
    }

    let before_tasks = tasks_by_id(before);
    let after_tasks = tasks_by_id(after);

    for (id, content) in &after_tasks {
        match before_tasks.get(id) {
            None => {
                return Err(bad(format!(
                    "an assistant may not create task block {id}; task declarations \
                     are the planner's and the user's to write"
                )));
            }
            Some(was) if was != content => {
                return Err(bad(format!(
                    "an assistant may not modify task block {id}; its declaration \
                     must survive the write byte-for-byte"
                )));
            }
            Some(_) => {}
        }
    }
    for id in before_tasks.keys() {
        if !after_tasks.contains_key(id) {
            return Err(bad(format!(
                "an assistant may not delete task block {id}; only the declaring \
                 author may retire a task"
            )));
        }
    }
    Ok(())
}

/// Enforce task provenance and user-only control after every report operation.
///
/// `block_delete_id` is the block a *block-level* delete op targeted (after
/// [`normalize_report_op`], so a user delete — which is rewritten into an
/// in-place tombstone — never reaches here as a delete). It is the one way a
/// live task declaration may leave the document: every other write shape
/// (whole-document `write`/`write_markdown`, upsert) that makes a live task
/// block disappear is rejected, for *every* author (#1179).
pub(crate) fn guard_task_declarations(
    before: &[ReportBlock],
    after: &[ReportBlock],
    author: EditAuthor,
    block_delete_id: Option<&str>,
) -> Result<(), CalmError> {
    let before_by_id: HashMap<_, _> = before.iter().map(|block| (&block.id, block)).collect();
    let after_by_id: HashMap<_, _> = after.iter().map(|block| (&block.id, block)).collect();

    for new in after.iter().filter(|block| is_task(block)) {
        let Some(old) = before_by_id.get(&new.id).filter(|block| is_task(block)) else {
            let expected = author_name(author)
                .ok_or_else(|| bad(format!("{author:?} may not create task blocks")))?;
            if string_field(new, "declared_by") != Some(expected)
                || (is_tombstone(new) && string_field(new, "tombstoned_by") != Some(expected))
            {
                return Err(bad(format!(
                    "new task block {} must attribute declared_by{} to {expected}",
                    new.id,
                    if is_tombstone(new) {
                        " and tombstoned_by"
                    } else {
                        ""
                    }
                )));
            }
            if author != EditAuthor::User
                && field(new, "released_by_user").is_some_and(|value| value != &Value::Bool(false))
            {
                return Err(bad("released_by_user may only be set by a user"));
            }
            continue;
        };

        if string_field(old, "declared_by") != string_field(new, "declared_by") {
            return Err(bad(format!(
                "task block {} declared_by is immutable",
                new.id
            )));
        }
        if string_field(old, "key") != string_field(new, "key") {
            return Err(bad(format!("task block {} key is immutable", new.id)));
        }
        if !is_tombstone(old) && is_tombstone(new) {
            let expected = author_name(author)
                .ok_or_else(|| bad(format!("{author:?} may not tombstone task blocks")))?;
            if string_field(new, "tombstoned_by") != Some(expected) {
                return Err(bad(format!(
                    "task block {} must attribute tombstoned_by to {expected}",
                    new.id
                )));
            }
        }
        if is_tombstone(old) {
            if !is_tombstone(new) {
                return Err(bad(format!(
                    "task tombstone {} may not be restored in place",
                    new.id
                )));
            }
            if string_field(old, "tombstoned_by") != string_field(new, "tombstoned_by") {
                return Err(bad(format!(
                    "task block {} tombstoned_by is immutable",
                    new.id
                )));
            }
        }
    }

    for old in before.iter().filter(|block| is_task(block)) {
        let next = after_by_id.get(&old.id).copied();
        if next.is_some_and(|block| !is_task(block)) {
            return Err(bad(format!(
                "task block {} may not change kind or drop declared_by",
                old.id
            )));
        }
        if author != EditAuthor::User
            && field(old, "released_by_user")
                != next.and_then(|block| field(block, "released_by_user"))
        {
            return Err(bad("released_by_user may only be changed by a user"));
        }
        let user_owned = string_field(old, "declared_by") == Some("user")
            || string_field(old, "tombstoned_by") == Some("user");
        // Moving is deliberately allowed: order is not block content, and MoveBlock does not
        // increment a block's revision.
        if author != EditAuthor::User && user_owned && next != Some(old) {
            return Err(bad(format!(
                "non-user author may not modify or delete user-controlled task block {}",
                old.id
            )));
        }
        if !is_tombstone(old) && !next.is_some_and(is_task) && block_delete_id != Some(&old.id) {
            let key = string_field(old, "key");
            // A whole-document write may retire a task only by carrying a
            // *fresh* tombstone for the same key attributed to the writer
            // itself — never to some other author (that is forgery, caught
            // above) and never by reusing a same-key tombstone that already
            // existed before the write. Reserved writers (kernel/plugin) have
            // no attribution name and so can never satisfy this: they fail
            // closed.
            let has_tombstone = author_name(author).is_some_and(|writer| {
                after.iter().any(|block| {
                    is_task(block)
                        && is_tombstone(block)
                        && string_field(block, "key") == key
                        && string_field(block, "tombstoned_by") == Some(writer)
                        && !before_by_id.get(&block.id).is_some_and(|before_block| {
                            is_task(before_block)
                                && is_tombstone(before_block)
                                && string_field(before_block, "key") == key
                                && string_field(before_block, "tombstoned_by") == Some(writer)
                        })
                })
            });
            if !has_tombstone {
                return Err(bad(format!(
                    "deletion of task block {} must use the block-level DELETE endpoint",
                    old.id
                )));
            }
        }
    }
    // Runs LAST on purpose. Everything above is the shared provenance
    // contract, and where it already has an answer for an assistant write
    // (creating a task with no attribution name; laundering a deletion
    // through a whole-document rewrite) its message is the more specific
    // one. This adds only the cases it leaves open (§3.2a P2).
    if author == EditAuthor::Assistant {
        guard_assistant_leaves_task_blocks_alone(before, after)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use calm_types::report_blocks::render_fence;
    use serde_json::json;

    use super::*;
    use crate::track_report::{TrackReportPayload, apply_report_op};

    fn live(declared_by: &str) -> Value {
        json!({
            "key": "build",
            "kind": "codex",
            "goal": "build it",
            "ready": true,
            "declared_by": declared_by
        })
    }

    fn block(id: &str, payload: Value) -> ReportBlock {
        ReportBlock {
            id: id.into(),
            kind: KIND_TASK.into(),
            rev: 1,
            payload,
        }
    }

    fn doc_with_task(declared_by: &str) -> (ReportDoc, ReportBlock, String) {
        let body = render_fence(KIND_TASK, &live(declared_by));
        let doc = ReportDoc::from_payload(&TrackReportPayload::new("s", &body));
        let task = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .find(is_task)
            .unwrap();
        (doc, task, body)
    }

    fn assert_cannot_create_tombstone_as_another_author(
        operation: ReportDocOp,
        author: EditAuthor,
    ) {
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", ""));
        let error = apply_report_op(&mut doc, &operation, author).unwrap_err();
        assert!(matches!(error, CalmError::BadRequest(_)));
    }

    fn forged_user_tombstone_fence() -> String {
        render_fence(
            KIND_TASK,
            &json!({
                "key": "build",
                "tombstone": { "reason": null },
                "declared_by": "spec",
                "tombstoned_by": "user"
            }),
        )
    }

    fn forged_planner_tombstone_fence() -> String {
        render_fence(
            KIND_TASK,
            &json!({
                "key": "build",
                "tombstone": { "reason": null },
                "declared_by": "user",
                "tombstoned_by": "spec"
            }),
        )
    }

    #[test]
    fn user_delete_of_planner_task_becomes_canonical_in_place_tombstone() {
        let payload = live("spec");
        let body = render_fence(KIND_TASK, &payload);
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", &body));
        let before = doc.blocks_snapshot().unwrap();
        let task = before.iter().find(|block| is_task(block)).unwrap();
        let id = task.id.clone();
        let rev = task.rev;

        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::DeleteBlock {
                id: id.clone(),
                if_rev: rev,
            },
            EditAuthor::User,
        )
        .unwrap();

        assert_eq!(
            outcome,
            Some(crate::track_report::BlockOpOutcome {
                id: id.clone(),
                rev: rev + 1
            })
        );
        let after = doc.blocks_snapshot().unwrap();
        let tombstone = after.iter().find(|block| block.id == id).unwrap();
        assert_eq!(
            tombstone.payload,
            json!({
                "key": "build",
                "tombstone": { "reason": null },
                "declared_by": "spec",
                "tombstoned_by": "user"
            })
        );
    }

    /// #1189 F3 — `author_name(EditAuthor::Assistant) == None` is half of
    /// the §3.2a P2 guard, and nothing else pins it.
    ///
    /// The block below attributes itself to `"assistant"`, which is the ONLY
    /// shape that can tell the two implementations apart: with the current
    /// fail-closed `None`, `guard_task_declarations` refuses before it ever
    /// compares `declared_by`; alias Assistant onto any name at all and the
    /// same call succeeds. Attributing the block to `"planner"` instead would
    /// keep failing under both (on the attribution mismatch), i.e. it would
    /// be a vacuous assertion.
    #[test]
    fn assistant_may_not_create_task_blocks() {
        let self_attributed = block("b_task", live("assistant"));
        let error = guard_task_declarations(
            &[],
            std::slice::from_ref(&self_attributed),
            EditAuthor::Assistant,
            None,
        )
        .expect_err("Assistant must not be able to declare a task block");
        let CalmError::BadRequest(message) = &error else {
            panic!("expected a 400, got {error:?}");
        };
        assert!(
            message.contains("may not create task blocks"),
            "the refusal must be the author_name fail-closed one, not an \
             attribution mismatch; got: {message}"
        );

        // Control: the same shape from Planner, self-attributed, goes through —
        // so the refusal above is about *who the author is*, not about task
        // creation being blocked for everyone.
        guard_task_declarations(
            &[],
            std::slice::from_ref(&block("b_task", live("spec"))),
            EditAuthor::Planner,
            None,
        )
        .expect("Planner may still declare its own task block");
    }

    /// #1189 §3.2a P2 — the gap the provenance table leaves open, and the
    /// case it must NOT close by accident.
    ///
    /// The first two assertions are the whole reason P2 exists as its own
    /// rule: `declared_by == "spec"` blocks are invisible to `#1180`'s
    /// user-only protection, and a live task's `ready`/`goal` feed
    /// dispatchability. The last one is the trap on the other side — a
    /// guard written as "no task blocks after an assistant write" would
    /// reject every prose edit made in a report that happens to hold a
    /// task list.
    #[test]
    fn assistant_may_not_touch_a_planner_declared_task_but_may_leave_it_alone() {
        let planner = block("b_task", live("spec"));
        let user = block("b_user_task", live("user"));

        let mut rewritten = planner.clone();
        rewritten.payload["goal"] = json!("assistant rewrite");
        let error = guard_task_declarations(
            std::slice::from_ref(&planner),
            &[rewritten],
            EditAuthor::Assistant,
            None,
        )
        .expect_err("an assistant may not rewrite a planner-declared task");
        let CalmError::BadRequest(message) = &error else {
            panic!("expected a 400, got {error:?}");
        };
        assert!(
            message.contains("may not modify task block"),
            "the refusal must be P2's, not an incidental attribution \
             mismatch: {message}"
        );

        // Deleting one — including through the block-level exemption the
        // #1179 rule grants every *other* author.
        let error = guard_task_declarations(
            std::slice::from_ref(&planner),
            &[],
            EditAuthor::Assistant,
            Some("b_task"),
        )
        .expect_err("an assistant may not use the block-level delete exemption");
        let CalmError::BadRequest(message) = &error else {
            panic!("expected a 400, got {error:?}");
        };
        assert!(message.contains("may not delete task block"), "{message}");
        // Control: the exemption is real for the planner.
        guard_task_declarations(
            std::slice::from_ref(&planner),
            &[],
            EditAuthor::Planner,
            Some("b_task"),
        )
        .expect("the block-level delete exemption still works for the planner");

        // And the positive: both declarations carried through untouched,
        // prose (which this diff does not even model) free to change.
        guard_task_declarations(
            &[planner.clone(), user.clone()],
            &[user, planner],
            EditAuthor::Assistant,
            None,
        )
        .expect(
            "task blocks that survive the write unchanged — in any order — \
             must not make the whole write fail",
        );
    }

    #[test]
    fn task_declaration_rules_reject_every_forbidden_transition() {
        let planner = block("b_task", live("spec"));
        let user = block("b_task", live("user"));
        let user_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{"reason":null},"declared_by":"spec","tombstoned_by":"user"}),
        );

        // 1: attribution is pinned to the writer; reserved writers fail closed.
        assert!(
            guard_task_declarations(&[], std::slice::from_ref(&user), EditAuthor::Planner, None)
                .is_err()
        );
        // #1189 — Assistant belongs in this list, not the "may author"
        // one: `author_name` gives it no name, so it can never satisfy the
        // `declared_by` attribution a new task block must carry.
        for author in [
            EditAuthor::Kernel,
            EditAuthor::Plugin,
            EditAuthor::Assistant,
        ] {
            assert!(
                guard_task_declarations(&[], std::slice::from_ref(&planner), author, None).is_err()
            );
        }
        // 2: declared_by cannot change, including the live -> tombstone transition.
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&planner),
                std::slice::from_ref(&user),
                EditAuthor::User,
                None
            )
            .is_err()
        );
        let changed_owner_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{},"declared_by":"user","tombstoned_by":"user"}),
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&planner),
                &[changed_owner_tombstone],
                EditAuthor::User,
                None
            )
            .is_err()
        );
        // 2b: a tombstone's author is immutable and it cannot revive in place.
        let planner_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{},"declared_by":"spec","tombstoned_by":"spec"}),
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&user_tombstone),
                &[planner_tombstone],
                EditAuthor::User,
                None
            )
            .is_err()
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&user_tombstone),
                std::slice::from_ref(&planner),
                EditAuthor::User,
                None
            )
            .is_err()
        );
        // 3: no non-user writer may modify/delete either form of user control.
        // #1189 — Assistant is a non-user writer like the rest, so the
        // user-control rule must bind it too.
        for author in [
            EditAuthor::Planner,
            EditAuthor::Kernel,
            EditAuthor::Plugin,
            EditAuthor::Assistant,
        ] {
            assert!(
                guard_task_declarations(std::slice::from_ref(&user), &[], author, None).is_err()
            );
            assert!(
                guard_task_declarations(std::slice::from_ref(&user_tombstone), &[], author, None)
                    .is_err()
            );
        }
        // 4': whole-document deletion cannot bypass the block delete rewrite.
        assert!(
            guard_task_declarations(std::slice::from_ref(&planner), &[], EditAuthor::User, None)
                .is_err()
        );
        let older_same_key_tombstone = block(
            "b_older_tombstone",
            json!({"key":"build","tombstone":{},"declared_by":"spec","tombstoned_by":"user"}),
        );
        assert!(
            guard_task_declarations(
                &[planner.clone(), older_same_key_tombstone.clone()],
                &[older_same_key_tombstone],
                EditAuthor::User,
                None
            )
            .is_err(),
            "an unrelated pre-existing same-key tombstone must not authorize deletion"
        );
        // 5: only a user may introduce or alter released_by_user.
        let mut released = planner.clone();
        released.payload["released_by_user"] = json!(true);
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&planner),
                &[released],
                EditAuthor::Planner,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn planner_cannot_modify_or_delete_user_task_through_any_write_shape() {
        let mut changed = live("user");
        changed["goal"] = json!("planner rewrite");
        let changed_fence = render_fence(KIND_TASK, &changed);

        let (mut doc, task, _) = doc_with_task("user");
        let operations = [
            ReportDocOp::Replace {
                summary: None,
                body: changed_fence.clone(),
                if_doc_rev: 0,
            },
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: changed_fence.clone(),
                if_doc_rev: 0,
            },
            ReportDocOp::UpsertBlock {
                id: Some(task.id.clone()),
                kind: KIND_TASK.into(),
                content: changed_fence.clone(),
                if_rev: Some(task.rev),
                if_doc_rev: None,
                position: None,
            },
            ReportDocOp::DeleteBlock {
                id: task.id.clone(),
                if_rev: task.rev,
            },
        ];
        for operation in operations {
            let mut attempt = ReportDoc::from_bytes(&doc.to_bytes()).unwrap();
            let error = apply_report_op(&mut attempt, &operation, EditAuthor::Planner).unwrap_err();
            assert!(matches!(error, CalmError::BadRequest(_)));
        }
    }

    #[test]
    fn replace_cannot_create_tombstone_attributed_to_another_author() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::Replace {
                summary: None,
                body: forged_user_tombstone_fence(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        );
    }

    #[test]
    fn write_markdown_cannot_create_tombstone_attributed_to_another_author() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: forged_user_tombstone_fence(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        );
    }

    #[test]
    fn upsert_block_cannot_create_tombstone_attributed_to_another_author() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::UpsertBlock {
                id: None,
                kind: KIND_TASK.into(),
                content: forged_user_tombstone_fence(),
                if_rev: None,
                if_doc_rev: Some(0),
                position: None,
            },
            EditAuthor::Planner,
        );
    }

    #[test]
    fn replace_cannot_create_planner_tombstone_as_user() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::Replace {
                summary: None,
                body: forged_planner_tombstone_fence(),
                if_doc_rev: 0,
            },
            EditAuthor::User,
        );
    }

    #[test]
    fn write_markdown_cannot_create_planner_tombstone_as_user() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: forged_planner_tombstone_fence(),
                if_doc_rev: 0,
            },
            EditAuthor::User,
        );
    }

    #[test]
    fn upsert_block_cannot_create_planner_tombstone_as_user() {
        assert_cannot_create_tombstone_as_another_author(
            ReportDocOp::UpsertBlock {
                id: None,
                kind: KIND_TASK.into(),
                content: forged_planner_tombstone_fence(),
                if_rev: None,
                if_doc_rev: Some(0),
                position: None,
            },
            EditAuthor::User,
        );
    }

    #[test]
    fn non_user_may_move_user_controlled_task_without_changing_its_revision() {
        let task_fence = render_fence(KIND_TASK, &live("user"));
        let body = format!("{task_fence}# trailing prose\n");
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", &body));
        let before = doc.blocks_snapshot().unwrap();
        let task = before.iter().find(|block| is_task(block)).unwrap().clone();

        apply_report_op(
            &mut doc,
            &ReportDocOp::MoveBlock {
                id: task.id.clone(),
                to_index: 1,
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .expect("order is not task content, so a non-user move is allowed");

        let after = doc.blocks_snapshot().unwrap();
        assert_eq!(after[1], task);
    }

    #[test]
    fn live_task_transition_cannot_forge_tombstone_author_or_change_key() {
        for (author, forged_by) in [(EditAuthor::Planner, "user"), (EditAuthor::User, "spec")] {
            let (mut doc, task, _) = doc_with_task("spec");
            let payload = json!({
                "key": "build",
                "tombstone": { "reason": null },
                "declared_by": "spec",
                "tombstoned_by": forged_by
            });
            let error = apply_report_op(
                &mut doc,
                &ReportDocOp::UpsertBlock {
                    id: Some(task.id),
                    kind: KIND_TASK.into(),
                    content: render_fence(KIND_TASK, &payload),
                    if_rev: Some(task.rev),
                    if_doc_rev: None,
                    position: None,
                },
                author,
            )
            .unwrap_err();
            assert!(matches!(error, CalmError::BadRequest(_)));
        }

        let (mut doc, task, _) = doc_with_task("spec");
        let mut renamed = live("spec");
        renamed["key"] = json!("renamed");
        let error = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some(task.id),
                kind: KIND_TASK.into(),
                content: render_fence(KIND_TASK, &renamed),
                if_rev: Some(task.rev),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(matches!(error, CalmError::BadRequest(_)));
    }

    #[test]
    fn user_whole_document_write_cannot_delete_live_task() {
        let (mut doc, _, _) = doc_with_task("spec");
        let error = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: "# replacement\n".into(),
                if_doc_rev: 0,
            },
            EditAuthor::User,
        )
        .unwrap_err();
        assert!(matches!(error, CalmError::BadRequest(_)));
    }

    /// #1179: the whole-document shapes drop a task fence silently unless the
    /// delete rule covers every author, not just the user.
    #[test]
    fn no_author_may_delete_a_live_task_through_a_whole_document_write() {
        // #1189 — "every author" has to mean every variant, Assistant
        // included; the whole point of #1179 was that an omitted author is
        // a silent hole.
        for author in [
            EditAuthor::User,
            EditAuthor::Planner,
            EditAuthor::Kernel,
            EditAuthor::Plugin,
            EditAuthor::Assistant,
        ] {
            for operation in [
                ReportDocOp::WriteMarkdown {
                    summary: None,
                    body: "# replacement\n".into(),
                    if_doc_rev: 0,
                },
                ReportDocOp::Replace {
                    summary: None,
                    body: "# replacement\n".into(),
                    if_doc_rev: 0,
                },
            ] {
                let (mut doc, _, _) = doc_with_task("spec");
                let error = apply_report_op(&mut doc, &operation, author).unwrap_err();
                let CalmError::BadRequest(message) = &error else {
                    panic!("{author:?} whole-document delete must be a 400: {error:?}");
                };
                assert!(
                    message.contains("block-level DELETE"),
                    "{author:?}: {message}"
                );
            }
        }
    }

    /// The sanctioned escape hatch stays open: a block-level delete of a task
    /// the author itself declared still goes through.
    #[test]
    fn block_level_delete_of_an_own_task_remains_allowed_for_every_author() {
        // #1189 — `Assistant` is deliberately ABSENT from this loop. The
        // escape hatch is not authorship-scoped (the task here is
        // declared_by "spec" for every author listed), so adding Assistant
        // would freeze "an assistant may delete a planner-declared live task
        // outright, no tombstone, no attribution" as a tested capability —
        // the mirror image of §3.2a P2, which says an assistant cannot even
        // declare a task block. Today `blocks.delete` is `require_role(Planner)`
        // so nothing reaches this guard as Assistant; when S2 opens the
        // block channel, the assistant's block-level delete right is an
        // explicit S2 decision, not something inherited by default from a
        // list this test happened to widen.
        for author in [EditAuthor::Planner, EditAuthor::Kernel, EditAuthor::Plugin] {
            let (mut doc, task, _) = doc_with_task("spec");
            apply_report_op(
                &mut doc,
                &ReportDocOp::DeleteBlock {
                    id: task.id.clone(),
                    if_rev: task.rev,
                },
                author,
            )
            .unwrap_or_else(|error| {
                panic!("{author:?} block-level delete must pass the guard: {error:?}")
            });
            assert!(
                doc.blocks_snapshot()
                    .unwrap()
                    .iter()
                    .all(|block| block.id != task.id)
            );
        }
        // The user's block-level delete is rewritten into an in-place
        // tombstone before the guard runs, so it keeps its own shape.
        let (mut doc, task, _) = doc_with_task("spec");
        apply_report_op(
            &mut doc,
            &ReportDocOp::DeleteBlock {
                id: task.id.clone(),
                if_rev: task.rev,
            },
            EditAuthor::User,
        )
        .expect("user block-level delete stays legal");
        let after = doc.blocks_snapshot().unwrap();
        let tombstone = after.iter().find(|block| block.id == task.id).unwrap();
        assert_eq!(tombstone.payload["tombstoned_by"], "user");
    }

    /// The exemption is scoped to the block the delete op named: a delete that
    /// also drops some *other* task block is still rejected.
    #[test]
    fn block_delete_exemption_covers_only_the_block_the_op_named() {
        let deleted = block("b_deleted", live("spec"));
        let collateral = block("b_collateral", live("spec"));
        let error = guard_task_declarations(
            &[deleted.clone(), collateral],
            std::slice::from_ref(&deleted),
            EditAuthor::Planner,
            Some("b_deleted"),
        )
        .unwrap_err();
        assert!(matches!(error, CalmError::BadRequest(_)));
    }

    /// A whole-document write may retire a task by carrying a fresh tombstone
    /// of its own — for the writer that signs it, and only for that writer.
    #[test]
    fn whole_document_retirement_needs_a_fresh_tombstone_signed_by_the_writer() {
        for (author, writer, other) in [
            (EditAuthor::User, "user", "spec"),
            (EditAuthor::Planner, "spec", "user"),
        ] {
            let old = block("b_task", live(writer));
            let mine = block(
                "b_tombstone",
                json!({"key":"build","tombstone":{"reason":null},"declared_by":writer,"tombstoned_by":writer}),
            );
            guard_task_declarations(
                std::slice::from_ref(&old),
                std::slice::from_ref(&mine),
                author,
                None,
            )
            .expect("a fresh self-signed tombstone retires the declaration");

            // Someone else's signature does not authorize this writer.
            let theirs = block(
                "b_tombstone",
                json!({"key":"build","tombstone":{"reason":null},"declared_by":writer,"tombstoned_by":other}),
            );
            assert!(
                guard_task_declarations(
                    std::slice::from_ref(&old),
                    std::slice::from_ref(&theirs),
                    author,
                    None,
                )
                .is_err(),
                "{author:?} must not lean on a {other} tombstone"
            );

            // A same-key tombstone that already existed is not a new one.
            let stale = block(
                "b_old_tombstone",
                json!({"key":"build","tombstone":{},"declared_by":writer,"tombstoned_by":writer}),
            );
            assert!(
                guard_task_declarations(
                    &[old.clone(), stale.clone()],
                    std::slice::from_ref(&stale),
                    author,
                    None,
                )
                .is_err(),
                "{author:?} must not reuse a pre-existing tombstone"
            );
        }
    }
}
