//! Shared task instructions for worker backends.

use serde_json::Value;

use super::Tx;
use super::task_verify_adapter::{GateStep, declared_gate_steps_tx};
use super::workspace_lease::worker::{CatchUpFacts, ReaderFacts, WorkerLeasePlan};
use crate::error::Result;

/// The prompt both worker adapters freeze in their prepare transaction: the attempt's lease plan
/// and its declared gate are read here, from the `tasks` row, never from the op payload.
pub(crate) async fn render_task_worker_prompt_tx(
    tx: &mut Tx<'_>,
    attempt_id: &str,
    goal: &str,
    context: &Value,
    acceptance: Option<&str>,
    plan: &WorkerLeasePlan,
) -> Result<String> {
    let gate = declared_gate_steps_tx(tx, attempt_id).await?;
    Ok(render_task_worker_prompt(
        attempt_id,
        goal,
        context,
        acceptance,
        plan.reader.as_ref(),
        plan.catch_up.as_ref(),
        gate.as_deref(),
    ))
}

/// The task prompt both worker adapters render. A read-only task (#1917) is told it shares the
/// checkout, which nothing enforces, and is given its repo, checkout, head and base (#1933). A
/// catch-up (#2058 D7) is told where it starts and what to replay. A gated task is shown its gate
/// steps to run before it reports (#2404).
fn render_task_worker_prompt(
    attempt_id: &str,
    goal: &str,
    context: &Value,
    acceptance: Option<&str>,
    reader: Option<&ReaderFacts>,
    catch_up: Option<&CatchUpFacts>,
    gate: Option<&[GateStep]>,
) -> String {
    let prompt = render_worker_prompt(goal, context, acceptance);
    let reader = reader.map(render_reader_facts).unwrap_or_default();
    let catch_up = catch_up.map(render_catch_up).unwrap_or_default();
    let gate = gate.map(render_gate_precheck).unwrap_or_default();
    format!(
        "{prompt}{reader}{catch_up}{gate}\n\nTask attempt_id: {attempt_id}\nEcho this exact attempt_id when reporting completion or failure."
    )
}

/// The worker runs the declared steps as its precheck; the kernel's run after the report stays
/// the verdict.
fn render_gate_precheck(steps: &[GateStep]) -> String {
    let mut out = String::from(
        "\n\nThis task has a gate. After you report done, the kernel runs these steps in order \
         from the checkout root, each under /bin/sh with no sandbox, on the changes it commits; \
         the first failing step fails the task. Before you report done, run every step yourself, \
         in order, from the checkout root. When a step fails because of the change, fix it; report \
         done only when no step fails that way. When a step cannot run here (sandbox, permissions, \
         a missing service or tool, or a check that needs the kernel's commit), still report done \
         and name the step and its error in your result: the kernel's run decides. When a step \
         fails for a reason the change does not cause, report the failure with the step name and \
         its output.",
    );
    for (index, step) in steps.iter().enumerate() {
        out.push_str(&format!(
            "\nGate step {} `{}`:\n```sh\n{}\n```",
            index + 1,
            step.name,
            step.cmd
        ));
    }
    out
}

/// #2058 D7: the worker cannot write git metadata (a codex worker's gitdir is read-only), so the
/// replay is a patch applied to the worktree, and the kernel's delivery commits it on `U`.
fn render_catch_up(facts: &CatchUpFacts) -> String {
    let CatchUpFacts {
        upstream_name,
        upstream,
        merge_base,
        work,
        head,
    } = facts;
    let mut out = format!(
        "\n\nThis task starts from the upstream `{upstream_name}` at `{upstream}`, fetched by the \
         kernel. Carry the track's work over: replay `git diff {merge_base} {work}` here and \
         resolve every conflict: `git apply --reject`, then `git merge-file` (versions from `git \
         show`) for each rejected file. `git apply -3`, cherry-pick and merge need a writable \
         gitdir. Leave no `.rej` files. Do not commit."
    );
    if head != work {
        out.push_str(&format!(
            " The checkout was at `{head}`; commits after `{work}` are not replayed."
        ));
    }
    out
}

fn render_reader_facts(reader: &ReaderFacts) -> String {
    let mut out = "\n\nThis task is read-only: do not modify the checkout (no edits, commits or \
                   new untracked files). Ignored build output may be written, but do not delete or \
                   reinstall what is there: other read-only tasks may be reading or building in \
                   it at the same time."
        .to_string();
    match &reader.repo {
        Ok(url) => out.push_str(&format!("\nrepo: {url}")),
        Err(why) => out.push_str(&format!("\nrepo: none ({why})")),
    }
    out.push_str(&format!("\ncheckout: {}", reader.checkout.display()));
    for (name, commit) in [("head", &reader.head), ("base", &reader.base)] {
        if let Some(commit) = commit {
            out.push_str(&format!("\n{name}: {commit}"));
        }
    }
    out
}

pub(crate) fn render_worker_prompt(
    goal: &str,
    context: &Value,
    acceptance_criteria: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("Goal:\n");
    out.push_str(goal);

    let context_str = match context {
        Value::Null => String::new(),
        Value::String(s) if s.trim().is_empty() => String::new(),
        Value::Object(m) if m.is_empty() => String::new(),
        Value::Array(a) if a.is_empty() => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    if !context_str.is_empty() {
        out.push_str("\n\nContext:\n");
        out.push_str(&context_str);
    }

    if let Some(ac) = acceptance_criteria.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n\nAcceptance criteria:\n");
        out.push_str(ac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_worker_turn_input_names_the_execution_id_attempt_id() {
        let out = render_task_worker_prompt("t:build", "g", &Value::Null, None, None, None, None);
        assert!(out.ends_with("\n\nTask attempt_id: t:build\nEcho this exact attempt_id when reporting completion or failure."), "{out}");
        assert!(
            !out.contains("idempotency") && !out.contains("task_id"),
            "{out}"
        );
    }

    #[test]
    fn a_gated_task_prompt_lists_its_steps_before_the_attempt_id() {
        let steps = [
            GateStep {
                name: "fmt".into(),
                cmd: "cargo fmt --check".into(),
            },
            GateStep {
                name: "test".into(),
                cmd: "cargo test -p x".into(),
            },
        ];
        let out =
            render_task_worker_prompt("t:build", "g", &Value::Null, None, None, None, Some(&steps));
        let fmt = out
            .find("\nGate step 1 `fmt`:\n```sh\ncargo fmt --check\n```")
            .expect(&out);
        let test = out
            .find("\nGate step 2 `test`:\n```sh\ncargo test -p x\n```")
            .expect(&out);
        let attempt = out.find("\n\nTask attempt_id: t:build").expect(&out);
        assert!(fmt < test && test < attempt, "{out}");
        assert!(out.contains("report done only when no step fails that way"), "{out}");
        assert!(
            out.contains("When a step cannot run here") && out.contains("still report done"),
            "{out}"
        );
    }

    #[test]
    fn render_worker_prompt_goal_only() {
        let out = render_worker_prompt("fix the bug", &Value::Null, None);
        assert_eq!(out, "Goal:\nfix the bug");
    }

    #[test]
    fn render_worker_prompt_goal_plus_context() {
        let ctx = serde_json::json!({ "issue": 42, "title": "x" });
        let out = render_worker_prompt("fix it", &ctx, None);
        assert!(out.starts_with("Goal:\nfix it"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"issue\": 42"));
        assert!(out.contains("\"title\": \"x\""));
        assert!(!out.contains("Acceptance criteria"));
    }

    #[test]
    fn render_worker_prompt_goal_plus_context_plus_ac() {
        let ctx = serde_json::json!({ "pr": 7 });
        let out = render_worker_prompt("ship", &ctx, Some("tests pass"));
        assert!(out.contains("Goal:\nship"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"pr\": 7"));
        assert!(out.ends_with("Acceptance criteria:\ntests pass"));
    }

    #[test]
    fn render_worker_prompt_skips_empty_context_object() {
        let out = render_worker_prompt("g", &serde_json::json!({}), Some("ac"));
        assert!(
            !out.contains("Context"),
            "empty {{}} should be skipped: {out}"
        );
        assert!(out.contains("Acceptance criteria:\nac"));
    }

    #[test]
    fn render_worker_prompt_skips_blank_ac() {
        let out = render_worker_prompt("g", &Value::Null, Some("   "));
        assert_eq!(out, "Goal:\ng");
    }
}
