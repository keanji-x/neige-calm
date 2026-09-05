//! Early diagnostics for newly authored gate requirements (#1492).
//!
//! This is deliberately not shell validation or a security boundary. We can
//! identify a direct Neige CLI call, but not what scripts, aliases or dynamic
//! commands will do. The verifier's empty environment remains authoritative.

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

/// Recognize only direct, literal calls, including a literal executable path.
/// Do not search arbitrary command text: `rg neige README.md` and quoted
/// fixture data are valid verification inputs, not kernel capability requests.
fn direct_kernel_cli(command: &str) -> bool {
    let Some(words) = shell::first_literal_command(command) else {
        return false;
    };
    let Some((executable, args)) = words.split_first() else {
        return false;
    };
    if executable.rsplit('/').next() != Some("neige") {
        return false;
    }
    let args: Vec<_> = args
        .iter()
        .map(String::as_str)
        .filter(|word| *word != "--json")
        .collect();
    if args.iter().any(|word| matches!(*word, "--help" | "-h")) {
        return false;
    }
    matches!(
        args.first().copied(),
        Some(
            "ls" | "cat"
                | "state"
                | "diff"
                | "cat-at"
                | "log"
                | "task-completed"
                | "task-failed"
                | "track-gc"
                | "vacuum"
        )
    )
}

#[cfg(test)]
mod tests {
    use calm_types::event::EditAuthor;
    use calm_types::report_blocks::render_fence;
    use serde_json::json;

    use crate::track_report::{ReportDocOp, TrackReportPayload, apply_report_op};
    use crate::track_report_doc::ReportDoc;

    use super::direct_kernel_cli;

    #[test]
    fn direct_gate_kernel_calls_are_distinguished_from_help_and_fixture_text() {
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
        ] {
            assert!(direct_kernel_cli(cmd), "missed {cmd}");
        }
        for cmd in [
            "neige --help",
            "neige cat --help",
            "neige cat '--help'",
            "neige cat --help|cat",
            "neige cat>/dev/null '--help'",
            "neige cat \"--help\"",
            "neige cat --he\\lp",
            "neige $command",
            "neige cat $(printf -- --help)",
            "neige --version",
            "neige help cat",
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
