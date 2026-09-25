//! Early diagnostics for newly authored gate requirements; deliberately not shell validation or a
//! security boundary — the verifier's empty environment remains authoritative.

use std::collections::HashMap;

use calm_types::report_blocks::KIND_TASK;
use calm_types::track_report::ReportBlock;

use crate::error::CalmError;

mod shell;

pub(crate) fn check_changed_task_gates(
    before: &[ReportBlock],
    after: &[ReportBlock],
) -> Result<(), CalmError> {
    let previous: HashMap<_, _> = before.iter().map(|block| (&block.id, block)).collect();
    for block in after.iter().filter(|block| block.kind == KIND_TASK) {
        // Preserve historical declarations byte-for-byte through prose edits,
        // reordering and withdrawal. Re-check only authored task content.
        if block.payload.get("tombstoned_by").is_some()
            || previous
                .get(&block.id)
                .is_some_and(|old| old.kind == KIND_TASK && old.payload == block.payload)
        {
            continue;
        }
        let Some(steps) = block
            .payload
            .pointer("/gate/steps")
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        for (index, step) in steps.iter().enumerate() {
            if step
                .get("cmd")
                .and_then(|v| v.as_str())
                .is_some_and(direct_kernel_cli)
            {
                return Err(CalmError::BadRequest(format!(
                    "task block {}: gate.steps[{index}].cmd invokes the Neige kernel CLI, \
                     but gates have no NEIGE_MCP_SOCKET or NEIGE_MCP_TOKEN. Verify files \
                     in the worker checkout (for example, python3 -m unittest discover); \
                     read plan output with neige from the Planner session instead",
                    block.id
                )));
            }
        }
    }
    Ok(())
}

/// One rule: a literal `neige` executable is a direct call, whatever its arguments, dynamic ones and
/// `--version` included; gates have no socket, so no such call can do its job there (#1801).
/// `rg neige README.md` and quoted fixture data are valid verification inputs, not kernel calls.
fn direct_kernel_cli(command: &str) -> bool {
    shell::first_literal_word(command)
        .is_some_and(|executable| executable.rsplit('/').next() == Some("neige"))
}

#[cfg(test)]
mod tests {
    use calm_types::event::EditAuthor;
    use calm_types::report_blocks::render_fence;
    use serde_json::json;

    use crate::track_report::{ReportDocOp, TrackReportPayload, apply_report_op};
    use crate::track_report_doc::ReportDoc;

    use super::direct_kernel_cli;

    /// #1801: a literal `neige` executable is flagged whatever its arguments; only a non-literal
    /// executable or a different command passes.
    #[test]
    fn every_direct_neige_call_is_a_kernel_call_including_help() {
        for cmd in [
            "neige cat plan/build/output",
            "neige --json state",
            "neige 'cat' plan/build/output",
            "neige state>/dev/null",
            "neige 2>/dev/null state",
            "neige 2>&1 state",
            "neige cat 'artifact with spaces'",
            "neige c\\at plan/build/output",
            "/usr/local/bin/neige ls plan",
            "'neige' task-completed --idempotency-key build",
            "neige cat plan/build/output | jq .",
            "neige --help",
            "neige cat --help",
            "neige cat '--help'",
            "neige cat --help|cat",
            "neige cat>/dev/null '--help'",
            "neige cat \"--help\"",
            "neige cat --he\\lp",
            "neige --version",
            "neige help cat",
            "neige snow",
            "neige",
            "neige $command",
            "neige cat \"$F\"",
            "neige cat $(printf -- --help)",
        ] {
            assert!(direct_kernel_cli(cmd), "missed {cmd}");
        }
        for cmd in [
            "$NEIGE cat plan/build/output",
            "$(which neige) state",
            "rg neige README.md",
            "printf '%s' 'neige cat plan/build/output'",
            "python3 -m unittest discover",
            "test -s artifacts/result.json",
            "./verify.sh",
            "sh -c 'neige state'",
        ] {
            assert!(!direct_kernel_cli(cmd), "misclassified {cmd}");
        }
    }

    #[test]
    fn historical_gate_survives_prose_edit_but_new_gate_is_checked_on_whole_document_write() {
        let fence = render_fence(
            "task",
            &json!({
                "key": "old", "kind": "codex", "goal": "Analyze", "declared_by": "spec", "ready": true,
                "gate": {"steps": [{"name": "check", "cmd": "neige cat plan/old/output"}]}
            }),
        );
        let body = format!("# Result\n\nOriginal analysis.\n\n{fence}");
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("", &body));
        let before = doc.blocks_snapshot().unwrap();
        let edited = body.replace("Original analysis.", "Updated analysis.");
        let rev = doc.doc_rev().unwrap();
        apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                body: edited.clone(),
                summary: None,
                if_doc_rev: rev,
            },
            EditAuthor::Planner,
        )
        .expect("historical gate must not block unrelated prose edits");
        let after = doc.blocks_snapshot().unwrap();
        let old = before.iter().find(|b| b.kind == "task").unwrap();
        assert!(
            after
                .iter()
                .any(|b| b.id == old.id && b.payload == old.payload)
        );

        let rev = doc.doc_rev().unwrap();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                body: format!("{edited}\n{}", fence.replace("old", "new")),
                summary: None,
                if_doc_rev: rev,
            },
            EditAuthor::Planner,
        )
        .expect_err("whole-document writes must also check new gates");
        assert!(err.to_string().contains("NEIGE_MCP_SOCKET"), "{err}");
    }
}
