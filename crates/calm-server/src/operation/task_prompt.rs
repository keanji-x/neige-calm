//! Shared task instructions for worker backends.

use serde_json::Value;

use super::Tx;
use super::task_gate_run::GateRunWait;
use super::task_verify_adapter::{GateStep, declared_gate_steps_tx};
use super::workspace_lease::worker::{CatchUpFacts, ReaderFacts, WorkerLeasePlan};
use crate::error::Result;
use crate::mcp_server::tools::task_gate::TOOL_TASK_GATE;

/// How a worker calls the kernel's tools: Codex through the neige MCP server, Claude through the
/// `neige` CLI from its shell (#2464 L7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerSurface {
    Mcp,
    Cli,
}

/// What the gate section of a worker's prompt needs besides the steps: the worker's surface and
/// the configured wait of one gate-run call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GateRunPrompt {
    pub surface: WorkerSurface,
    pub wait: GateRunWait,
}

/// The prompt both worker adapters freeze in their prepare transaction: the attempt's lease plan
/// and its declared gate are read here, from the `tasks` row, never from the op payload.
pub(crate) async fn render_task_worker_prompt_tx(
    tx: &mut Tx<'_>,
    attempt_id: &str,
    goal: &str,
    context: &Value,
    acceptance: Option<&str>,
    plan: &WorkerLeasePlan,
    gate_run: GateRunPrompt,
) -> Result<String> {
    let gate = declared_gate_steps_tx(tx, attempt_id).await?;
    Ok(render_task_worker_prompt(
        attempt_id,
        goal,
        context,
        acceptance,
        plan.reader.as_ref(),
        plan.catch_up.as_ref(),
        gate.as_deref().map(|steps| (steps, gate_run)),
    ))
}

/// The task prompt both worker adapters render. A read-only task (#1917) is told it shares the
/// checkout, which nothing enforces, and is given its repo, checkout, head and base (#1933). A
/// catch-up (#2058 D7) is told where it starts and what to replay. A gated task is shown its gate
/// steps (#2404) and, when the kernel commits its checkout, how to have the kernel run them (#2464).
fn render_task_worker_prompt(
    attempt_id: &str,
    goal: &str,
    context: &Value,
    acceptance: Option<&str>,
    reader: Option<&ReaderFacts>,
    catch_up: Option<&CatchUpFacts>,
    gate: Option<(&[GateStep], GateRunPrompt)>,
) -> String {
    let prompt = render_worker_prompt(goal, context, acceptance);
    let gate = gate
        .map(|(steps, gate_run)| {
            // A read-only task holds no kernel-delivery lease: nothing to commit, so its gate runs
            // only after the report.
            let run = reader.is_none().then_some(gate_run);
            render_gate(attempt_id, steps, run)
        })
        .unwrap_or_default();
    let reader = reader.map(render_reader_facts).unwrap_or_default();
    let catch_up = catch_up.map(render_catch_up).unwrap_or_default();
    format!(
        "{prompt}{reader}{catch_up}{gate}\n\nTask attempt_id: {attempt_id}\nEcho this exact attempt_id when reporting completion or failure."
    )
}

/// The gate section (#2464 §6): the kernel runs the steps, on the worker's request when it commits
/// the checkout (`run`), otherwise after the report. The worker never runs them in its sandbox.
fn render_gate(attempt_id: &str, steps: &[GateStep], run: Option<GateRunPrompt>) -> String {
    let mut out = match run {
        Some(GateRunPrompt { surface, wait }) => {
            let (tool, message) = match surface {
                WorkerSurface::Mcp => (format!("`{TOOL_TASK_GATE}`"), "`commit_message`"),
                WorkerSurface::Cli => (
                    format!("`neige task gate --attempt-id {attempt_id}`"),
                    "`--commit-message`",
                ),
            };
            format!(
                "\n\nThis task has a gate: the steps below run in order from the checkout root \
                 under /bin/sh, outside your sandbox. To run them, call {tool} with {message}: the \
                 full message of the commit (what the repository requires: subject, body, \
                 trailers). The kernel makes your current changes the one commit of this attempt, \
                 with that message, and runs the gate on it here. It answers within {}; while it \
                 says `running`, call it again. Do not edit files while a run is in progress. Fix \
                 what a failing step reports and run again. If the last run passed and you change \
                 nothing after it, that run is the gate's verdict when you report done, and its \
                 commit is what the kernel delivers. Otherwise the kernel runs the gate after you \
                 report. You need not run these steps yourself.",
                wait.render()
            )
        }
        None => "\n\nThis task has a gate: after you report done, the kernel runs these steps in \
                 order from the checkout root under /bin/sh, outside your sandbox."
            .to_string(),
    };
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

    fn steps() -> [GateStep; 2] {
        [
            GateStep {
                name: "fmt".into(),
                cmd: "cargo fmt --check".into(),
            },
            GateStep {
                name: "test".into(),
                cmd: "cargo test -p x".into(),
            },
        ]
    }

    fn gate_run(surface: WorkerSurface) -> GateRunPrompt {
        GateRunPrompt {
            surface,
            wait: GateRunWait::DEFAULT,
        }
    }

    /// P1 (#2464): a gated task with a kernel commit is told to have the kernel run its gate, by
    /// its surface's spelling of the tool, with the configured wait; the sandbox precheck is gone.
    #[test]
    fn a_gated_task_prompt_names_the_gate_run_tool_and_its_wait() {
        let steps = steps();
        for (surface, tool, message) in [
            (WorkerSurface::Mcp, "`neige_task_gate`", "`commit_message`"),
            (
                WorkerSurface::Cli,
                "`neige task gate --attempt-id t:build`",
                "`--commit-message`",
            ),
        ] {
            let out = render_task_worker_prompt(
                "t:build",
                "g",
                &Value::Null,
                None,
                None,
                None,
                Some((&steps, gate_run(surface))),
            );
            assert!(
                out.contains(&format!(
                    "To run them, call {tool} with {message}: the full message"
                )),
                "{out}"
            );
            assert!(out.contains("It answers within 90 seconds;"), "{out}");
            assert!(out.contains("outside your sandbox"), "{out}");
            assert!(
                out.contains(
                    "If the last run passed and you change nothing after it, that run is the \
                     gate's verdict when you report done"
                ),
                "{out}"
            );
            assert!(
                out.contains("You need not run these steps yourself."),
                "{out}"
            );
            assert!(!out.contains("run every step yourself"), "{out}");
            assert!(!out.contains("When a step cannot run here"), "{out}");
            let fmt = out
                .find("\nGate step 1 `fmt`:\n```sh\ncargo fmt --check\n```")
                .expect(&out);
            let test = out
                .find("\nGate step 2 `test`:\n```sh\ncargo test -p x\n```")
                .expect(&out);
            let attempt = out.find("\n\nTask attempt_id: t:build").expect(&out);
            assert!(fmt < test && test < attempt, "{out}");
        }
        let short = render_task_worker_prompt(
            "t:build",
            "g",
            &Value::Null,
            None,
            None,
            None,
            Some((
                &steps,
                GateRunPrompt {
                    surface: WorkerSurface::Mcp,
                    wait: GateRunWait::for_idle(std::time::Duration::from_secs(60)),
                },
            )),
        );
        assert!(short.contains("It answers within 30 seconds;"), "{short}");
    }

    /// P1: a read-only gated task has nothing for the kernel to commit, so it is told only that the
    /// kernel runs its gate after the report.
    #[test]
    fn a_read_only_gated_task_prompt_has_no_gate_run() {
        let steps = steps();
        let reader = ReaderFacts {
            repo: Err("no remote".into()),
            checkout: std::path::PathBuf::from("/checkout"),
            head: None,
            base: None,
        };
        let out = render_task_worker_prompt(
            "t:build",
            "g",
            &Value::Null,
            None,
            Some(&reader),
            None,
            Some((&steps, gate_run(WorkerSurface::Mcp))),
        );
        assert!(
            out.contains(
                "This task has a gate: after you report done, the kernel runs these steps in \
                 order from the checkout root under /bin/sh, outside your sandbox.\nGate step 1"
            ),
            "{out}"
        );
        assert!(!out.contains("neige_task_gate"), "{out}");
        assert!(!out.contains("run every step yourself"), "{out}");
    }

    /// L1: the wait stays under half the idle window and never above 90 s.
    #[test]
    fn the_wait_bound_stays_under_the_idle_window() {
        use std::time::Duration;
        assert_eq!(
            GateRunWait::for_idle(Duration::from_secs(60)).duration(),
            Duration::from_secs(30)
        );
        assert_eq!(
            GateRunWait::for_idle(Duration::from_secs(3600)).duration(),
            Duration::from_secs(90)
        );
        assert_eq!(GateRunWait::DEFAULT.duration(), Duration::from_secs(90));
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
