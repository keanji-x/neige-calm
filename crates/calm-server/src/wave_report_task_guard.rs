//! Writer-attribution guards for task declaration blocks (#985 PR2-a).

use std::collections::HashMap;

use calm_types::event::EditAuthor;
use calm_types::report_blocks::{KIND_TASK, render_fence};
use calm_types::wave_report::ReportBlock;
use serde_json::{Value, json};

use crate::error::CalmError;
use crate::wave_report::ReportDocOp;
use crate::wave_report_doc::ReportDoc;

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

fn author_name(author: EditAuthor) -> Option<&'static str> {
    match author {
        EditAuthor::Spec => Some("spec"),
        EditAuthor::User => Some("user"),
        EditAuthor::Kernel | EditAuthor::Plugin => None,
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
            "wave_report: snapshot for task delete rewrite: {error}"
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

/// Enforce task provenance and user-only control after every report operation.
pub(crate) fn guard_task_declarations(
    before: &[ReportBlock],
    after: &[ReportBlock],
    author: EditAuthor,
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
        if author == EditAuthor::User && !is_tombstone(old) && !next.is_some_and(is_task) {
            let key = string_field(old, "key");
            let has_tombstone = after.iter().any(|block| {
                is_task(block)
                    && is_tombstone(block)
                    && string_field(block, "key") == key
                    && string_field(block, "tombstoned_by") == Some("user")
                    && !before_by_id.get(&block.id).is_some_and(|before_block| {
                        is_task(before_block)
                            && is_tombstone(before_block)
                            && string_field(before_block, "key") == key
                            && string_field(before_block, "tombstoned_by") == Some("user")
                    })
            });
            if !has_tombstone {
                return Err(bad(format!(
                    "user deletion of task block {} must use the block-level DELETE endpoint",
                    old.id
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use calm_types::report_blocks::render_fence;
    use serde_json::json;

    use super::*;
    use crate::wave_report::{WaveReportPayload, apply_report_op};

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
        let doc = ReportDoc::from_payload(&WaveReportPayload::new("s", &body));
        let task = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .find(is_task)
            .unwrap();
        (doc, task, body)
    }

    #[test]
    fn user_delete_of_spec_task_becomes_canonical_in_place_tombstone() {
        let payload = live("spec");
        let body = render_fence(KIND_TASK, &payload);
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", &body));
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
            Some(crate::wave_report::BlockOpOutcome {
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

    #[test]
    fn task_declaration_rules_reject_every_forbidden_transition() {
        let spec = block("b_task", live("spec"));
        let user = block("b_task", live("user"));
        let user_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{"reason":null},"declared_by":"spec","tombstoned_by":"user"}),
        );

        // 1: attribution is pinned to the writer; reserved writers fail closed.
        assert!(
            guard_task_declarations(&[], std::slice::from_ref(&user), EditAuthor::Spec).is_err()
        );
        for author in [EditAuthor::Kernel, EditAuthor::Plugin] {
            assert!(guard_task_declarations(&[], std::slice::from_ref(&spec), author).is_err());
        }
        // 2: declared_by cannot change, including the live -> tombstone transition.
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&spec),
                std::slice::from_ref(&user),
                EditAuthor::User
            )
            .is_err()
        );
        let changed_owner_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{},"declared_by":"user","tombstoned_by":"user"}),
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&spec),
                &[changed_owner_tombstone],
                EditAuthor::User
            )
            .is_err()
        );
        // 2b: a tombstone's author is immutable and it cannot revive in place.
        let spec_tombstone = block(
            "b_task",
            json!({"key":"build","tombstone":{},"declared_by":"spec","tombstoned_by":"spec"}),
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&user_tombstone),
                &[spec_tombstone],
                EditAuthor::User
            )
            .is_err()
        );
        assert!(
            guard_task_declarations(
                std::slice::from_ref(&user_tombstone),
                std::slice::from_ref(&spec),
                EditAuthor::User
            )
            .is_err()
        );
        // 3: no non-user writer may modify/delete either form of user control.
        for author in [EditAuthor::Spec, EditAuthor::Kernel, EditAuthor::Plugin] {
            assert!(guard_task_declarations(std::slice::from_ref(&user), &[], author).is_err());
            assert!(
                guard_task_declarations(std::slice::from_ref(&user_tombstone), &[], author)
                    .is_err()
            );
        }
        // 4': whole-document deletion cannot bypass the block delete rewrite.
        assert!(
            guard_task_declarations(std::slice::from_ref(&spec), &[], EditAuthor::User).is_err()
        );
        let older_same_key_tombstone = block(
            "b_older_tombstone",
            json!({"key":"build","tombstone":{},"declared_by":"spec","tombstoned_by":"user"}),
        );
        assert!(
            guard_task_declarations(
                &[spec.clone(), older_same_key_tombstone.clone()],
                &[older_same_key_tombstone],
                EditAuthor::User
            )
            .is_err(),
            "an unrelated pre-existing same-key tombstone must not authorize deletion"
        );
        // 5: only a user may introduce or alter released_by_user.
        let mut released = spec.clone();
        released.payload["released_by_user"] = json!(true);
        assert!(
            guard_task_declarations(std::slice::from_ref(&spec), &[released], EditAuthor::Spec)
                .is_err()
        );
    }

    #[test]
    fn spec_cannot_modify_or_delete_user_task_through_any_write_shape() {
        let mut changed = live("user");
        changed["goal"] = json!("spec rewrite");
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
            let error = apply_report_op(&mut attempt, &operation, EditAuthor::Spec).unwrap_err();
            assert!(matches!(error, CalmError::BadRequest(_)));
        }
    }

    #[test]
    fn non_user_may_move_user_controlled_task_without_changing_its_revision() {
        let task_fence = render_fence(KIND_TASK, &live("user"));
        let body = format!("{task_fence}# trailing prose\n");
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", &body));
        let before = doc.blocks_snapshot().unwrap();
        let task = before.iter().find(|block| is_task(block)).unwrap().clone();

        apply_report_op(
            &mut doc,
            &ReportDocOp::MoveBlock {
                id: task.id.clone(),
                to_index: 1,
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
        )
        .expect("order is not task content, so a non-user move is allowed");

        let after = doc.blocks_snapshot().unwrap();
        assert_eq!(after[1], task);
    }

    #[test]
    fn live_task_transition_cannot_forge_tombstone_author_or_change_key() {
        for (author, forged_by) in [(EditAuthor::Spec, "user"), (EditAuthor::User, "spec")] {
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
            EditAuthor::Spec,
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
}
