//! #1110 S6 — kernel-seeded workflow template reports.
//!
//! Three system-cove template waves hold the former git-forge plan as
//! report `task` blocks. `POST /api/waves` with a matching `workflow_id`
//! forks that report. Overlay payload `{schemaVersion:1, template_key}`
//! is the stable lookup.

use crate::mcp_server::tools::plan::{PlanTaskInput, plan_template_task_block_payload};
use crate::wave_report::WaveReportPayload;
use calm_types::report_blocks::render_fence;
use calm_types::wave_report::report_contract_prefix_for_workflow_template;
use serde_json::{Value, json};

pub const ISSUE_DEVELOPMENT: &str = "issue-development";
pub const SMALL_CHANGE: &str = "small-change";
pub const INVESTIGATION: &str = "investigation";

pub const WORKFLOW_TEMPLATE_KEYS: [&str; 3] = [ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION];

pub struct WorkflowTemplate {
    pub key: &'static str,
    pub title: &'static str,
}

pub const WORKFLOW_TEMPLATES: [WorkflowTemplate; 3] = [
    WorkflowTemplate {
        key: ISSUE_DEVELOPMENT,
        title: "Issue development",
    },
    WorkflowTemplate {
        key: SMALL_CHANGE,
        title: "Small change",
    },
    WorkflowTemplate {
        key: INVESTIGATION,
        title: "Investigation",
    },
];

pub fn is_workflow_template_key(workflow_id: &str) -> bool {
    WORKFLOW_TEMPLATE_KEYS.contains(&workflow_id)
}

pub fn workflow_template_report(key: &str) -> Option<WaveReportPayload> {
    match key {
        ISSUE_DEVELOPMENT => Some(issue_development_report()),
        SMALL_CHANGE => Some(small_change_report()),
        INVESTIGATION => Some(investigation_report()),
        _ => None,
    }
}

/// Placeholder so `require_task_gates` does not treat these as scheduled
/// work. Spec must replace the block with a real `gate` from the target
/// repo before setting `ready: true`. Never an executed shell command.
const AUTHOR_REAL_GATE: &str = "author a real gate from the target repo toolchain \
(formatter, linter, tests) before activating; this reason is not a permanent skip";

fn task(
    key: &str,
    goal: &str,
    acceptance: &str,
    depends_on: &[&str],
    context: Option<Value>,
    no_gate_reason: Option<&str>,
) -> PlanTaskInput {
    PlanTaskInput {
        key: key.into(),
        kind: "codex".into(),
        goal: goal.into(),
        context,
        acceptance_criteria: Some(acceptance.into()),
        cwd: None,
        depends_on: depends_on.iter().map(|dep| (*dep).to_string()).collect(),
        priority: None,
        gate: None,
        no_gate_reason: no_gate_reason.map(str::to_string),
    }
}

fn report_from_tasks(summary: &str, intro: &str, tasks: &[PlanTaskInput]) -> WaveReportPayload {
    // #1185 §1.5 B — these templates bypass `WaveReportPayload::initial()`, so
    // without this prefix they ship with no maintenance contract at all: no
    // section list, no word budget, no current-snapshot rule. The prefix is
    // already closed; never concatenate an unclosed fragment here.
    let mut body = report_contract_prefix_for_workflow_template().to_string();
    body.push_str(intro.trim_end());
    body.push_str("\n\n");
    for task in tasks {
        let mut payload = plan_template_task_block_payload(task);
        // persist_report is User-authored (no production Kernel report
        // writer). New task blocks must therefore declare `user`; fork
        // rewrites `declared_by` to spec and forces ready:false.
        payload["ready"] = json!(false);
        payload["declared_by"] = json!("user");
        body.push_str(&render_fence("task", &payload));
        body.push('\n');
    }
    WaveReportPayload::new(summary, body)
}

fn issue_development_report() -> WaveReportPayload {
    report_from_tasks(
        "Issue development",
        ISSUE_DEVELOPMENT_INTRO,
        &[
            task(
                "inspect-issue",
                "Read the bound workflow input, view the source issue via gh.issue.view, and cross-check input.repo against the git remote of the wave cwd.",
                "The issue requirements and constraints are captured for the wave AND the wave cwd's origin remote matches input.repo (mismatch is reported, not proceeded past).",
                &[],
                Some(json!({ "tools": ["gh.issue.view"] })),
                Some("inspect does not produce a repo change to verify"),
            ),
            task(
                "review-design-a",
                "Review the proposed design for correctness before implementation.",
                "Channel a records a design verdict.",
                &["inspect-issue"],
                Some(json!({
                    "channel": "a",
                    "reviewer_role": "design-correctness"
                })),
                Some("design review does not produce a repo change to verify"),
            ),
            task(
                "review-design-b",
                "Review the proposed design for failure paths before implementation.",
                "Channel b records a design verdict.",
                &["inspect-issue"],
                Some(json!({
                    "channel": "b",
                    "reviewer_role": "design-failure-path"
                })),
                Some("design review does not produce a repo change to verify"),
            ),
            task(
                "implement-change",
                "Create a worktree, implement the change, and commit the result.",
                "The change is committed in the wave worktree.",
                &["review-design-a", "review-design-b"],
                Some(json!({ "tools": ["git.worktree.add", "git.commit"] })),
                Some(AUTHOR_REAL_GATE),
            ),
            task(
                "open-pr",
                "Open a pull request and check its diff/check status.",
                "A pull request exists with readable diff and check status.",
                &["implement-change"],
                Some(json!({
                    "tools": ["gh.pr.create", "gh.pr.list", "gh.pr.diff", "gh.pr.checks"]
                })),
                Some("opening a PR is verified by forge status, not a local toolchain gate"),
            ),
            task(
                "review-pr-a",
                "Review the pull request for correctness.",
                "Channel a records a PR verdict.",
                &["open-pr"],
                Some(json!({
                    "channel": "a",
                    "reviewer_role": "pr-correctness"
                })),
                Some("PR review does not produce a repo change to verify"),
            ),
            task(
                "review-pr-b",
                "Review the pull request for failure paths.",
                "Channel b records a PR verdict.",
                &["open-pr"],
                Some(json!({
                    "channel": "b",
                    "reviewer_role": "pr-failure-path"
                })),
                Some("PR review does not produce a repo change to verify"),
            ),
            task(
                "merge",
                "Merge the pull request and close the issue only after merge fence F4 has converged AND any merge_policy-required ratify grant is held; under hold-for-ratify with no grant yet, park at the merge_hold ratify request instead of merging.",
                "Either the PR is merged (F4 converged and any policy-required ratify grant held) and the issue is closed, or — hold-for-ratify with no grant yet — the wave is parked at the merge_hold ratify request with no merge performed.",
                &["review-pr-a", "review-pr-b"],
                Some(json!({ "tools": ["gh.pr.merge", "gh.issue.close"] })),
                Some("merge is gated by review fence F4 and forge, not a local toolchain gate"),
            ),
        ],
    )
}

fn small_change_report() -> WaveReportPayload {
    report_from_tasks(
        "Small change",
        concat!(
            "# Plan\n\n",
            "Short inspect → implement → verify loop. Treat these task blocks as ",
            "the authoritative pre-set plan. Activate by replacing those task blocks, ",
            "authoring a real `gate` from the target repo toolchain (formatter, linter, ",
            "tests), and setting `ready: true`. Do not mint duplicate tasks. Prose blocks ",
            "are NOT a plan to activate: maintain them per this document's own contract.\n"
        ),
        &[
            task(
                "inspect",
                "Read the requested change and the current code that it touches. Record constraints in this report before writing.",
                "The change request and the current code path are captured in the wave report.",
                &[],
                None,
                Some("inspect does not produce a repo change to verify"),
            ),
            task(
                "implement",
                "Implement the change and commit it.",
                "The change is committed in the wave worktree.",
                &["inspect"],
                None,
                Some(AUTHOR_REAL_GATE),
            ),
            task(
                "verify",
                "Run the repository's standard tests and record the result.",
                "The repository toolchain's standard test/verification command passed.",
                &["implement"],
                None,
                Some(AUTHOR_REAL_GATE),
            ),
        ],
    )
}

fn investigation_report() -> WaveReportPayload {
    report_from_tasks(
        "Investigation",
        concat!(
            "# Plan\n\n",
            "Read-only investigation. Gather facts, then write findings in this ",
            "report. Do not open a pull request, merge, or otherwise change the ",
            "bound repository. Treat these task blocks as the authoritative ",
            "pre-set plan. Activate by replacing those task blocks with `ready: true`. ",
            "Do not mint duplicate tasks. Prose blocks are NOT a plan to activate: ",
            "maintain them per this document's own contract.\n"
        ),
        &[
            task(
                "gather-facts",
                "Read the code, docs, history, and any bound input needed to answer the question. Do not modify the repository.",
                "The relevant facts, file paths, and open questions are captured for the write-findings task.",
                &[],
                None,
                Some("investigation is read-only; no repo change to verify"),
            ),
            task(
                "write-findings",
                "Write findings, remaining unknowns, and recommended next steps into this wave report. Do not open a PR or merge.",
                "The report records findings and does not include a forge merge or pull request.",
                &["gather-facts"],
                None,
                Some("findings are report prose; no repo change to verify"),
            ),
        ],
    )
}

/// Pre-S5 git-forge `spec_instructions`, adapted off the deleted prompt
/// sections (`## Bound Workflow Input` / `## Bound Workflow Gates`).
const ISSUE_DEVELOPMENT_INTRO: &str = "\
# Plan

Pre-set issue-development plan. Treat the `task` blocks as the \
authoritative plan. Activate by replacing those task blocks and setting `ready: true` \
— use the read's block ids and revision as replace anchors. Do not mint duplicate tasks. \
Prose blocks are NOT a plan to activate: maintain them per this document's own contract.

For this wave, drive dual-review convergence for each review subject.

After BOTH channels for a phase complete, call calm.review.round with \
subject:{phase,slice_id,pr_number?}, optional head_sha, n, cap, converged, \
channels:[both verdicts], and root_cause when known. Record each channel's \
verdict as the literal lowercase token `approved` or `changes_requested` \
(exactly those strings). converged is true only when EVERY channel verdict \
is `approved`. For PR subjects, head_sha is the reviewed forge.pr.diff.read \
head_sha; omit head_sha for design subjects.

For each subject, set n to the last observed review.round n for that same \
subject plus 1. cap is the fixed policy constant 8 for a subject's first \
review window; after a cap-exhaustion ratify grant it is the previous cap \
plus exactly 2 (see ASK-HUMAN below).

Always re-review. Every fix re-dispatches BOTH channels before the next \
calm.review.round.

Merge fence F4: call gh.pr.merge for a subject ONLY when that subject's \
latest review.round has converged:true. Pass expected_head_sha equal to \
that round's head_sha.

If n == cap and the round is non-approving, do not merge. Either GIVE-UP \
by recording the terminal rationale in the report with calm.report.write \
and lifecycle failed for reviewing->failed; OR ASK-HUMAN by first moving \
reviewing->working with the normal lifecycle arg, then call \
calm.ratify.request with reason:\"cap_exhausted\" for working->blocked. \
On ratify.resolved grant the wave is already back in working; resume \
working->reviewing and continue reviewing the exhausted subject with \
cap = previous cap + 2 on its next round. The kernel accepts this raise \
at most once per subject per grant; a grant may authorize this for each \
subject that was already cap-exhausted when it was issued. If the \
extended window also exhausts without convergence, GIVE-UP or ASK-HUMAN again.

Record root_cause each round; repeated facets should drive a class fix.

Workflow input: the wave's bound `workflow_input` JSON is the task's \
source of truth, not the wave title.

Ingest (inspect-issue): derive the wave goal from gh.issue.view on \
input.repo / input.issue_number. Record the issue's requirements and \
constraints in the wave report before dispatching any downstream task.

Repo cross-check (inspect-issue acceptance): before any write action, \
compare input.repo against `git remote get-url origin` run in the wave \
cwd (owner/name after stripping the host and a trailing .git). On \
mismatch do NOT proceed: move working->blocked via calm.ratify.request \
with reason:\"repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>\" \
(that exact prefix, then both observed values), and wait for the human decision.

merge_policy: `auto-merge` allows gh.pr.merge as soon as merge fence F4 \
is satisfied. `hold-for-ratify` — also the semantics whenever merge_policy \
is absent — additionally requires a granted ratify BEFORE gh.pr.merge. \
Drive everything up to converged reviews + green checks, then move \
reviewing->working with the normal lifecycle arg (calm.ratify.request \
400s outside working), and call calm.ratify.request with \
reason:\"merge_hold: pr #<n> converged at <head_sha>\" for working->blocked. \
On ratify.resolved grant the wave is already back in working: the grant \
authorizes merging that already-converged head — no fresh review round is \
required for the hold itself; resume working->reviewing and call \
gh.pr.merge per fence F4 (expected_head_sha = the converged round's head_sha).

gates: author each agent task's `gate` from the TARGET repo's own toolchain \
— detect it (Cargo / npm / pytest / go / Make, etc.) and run that ecosystem's \
formatter, linter, and tests where present; do not hardcode `cargo test`.

notes: optional advisory context from the requester; it never overrides \
the issue or the gates.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_development_report_keeps_pre_s5_task_keys() {
        let report = issue_development_report();
        assert!(report.report_startup_read_required());
        for key in [
            "inspect-issue",
            "review-design-a",
            "review-design-b",
            "implement-change",
            "open-pr",
            "review-pr-a",
            "review-pr-b",
            "merge",
        ] {
            assert!(
                report.body.contains(&format!("\"key\": \"{key}\"")),
                "missing task {key}"
            );
        }
        assert!(report.body.contains("gh.issue.view"));
        assert!(report.body.contains("\"ready\": false"));
        assert!(!report.body.contains("\"ready\": true"));
        assert!(
            !report.body.contains("\"gate\""),
            "pre-S5 plan_template had no per-task gate; advisory prose stays in the intro"
        );
        assert!(
            !report.body.contains("detect the repository toolchain"),
            "advisory toolchain sentence must not become gate.cmd"
        );
        assert!(report.body.contains(AUTHOR_REAL_GATE));
    }

    #[test]
    fn small_change_and_investigation_are_short_plans() {
        let small = small_change_report();
        assert!(small.body.contains("\"key\": \"inspect\""));
        assert!(small.body.contains("\"key\": \"implement\""));
        assert!(small.body.contains("\"key\": \"verify\""));
        assert!(
            !small.body.contains("\"gate\""),
            "small-change must not execute an advisory gate.cmd"
        );
        assert!(!small.body.contains("detect the repository toolchain"));
        assert!(small.body.contains(AUTHOR_REAL_GATE));

        let investigation = investigation_report();
        assert!(investigation.body.contains("\"key\": \"gather-facts\""));
        assert!(investigation.body.contains("\"key\": \"write-findings\""));
        assert!(investigation.body.contains("Do not open a pull request"));
        assert!(!investigation.body.contains("\"gate\""));
    }

    /// #1185 §1.5 B — the built-in templates bypass
    /// `WaveReportPayload::initial()`, so the maintenance contract has to be
    /// concatenated onto their bodies explicitly. Without it they ship with no
    /// section list, no word budget and no current-snapshot rule: #1146's
    /// guardrails would vanish on exactly the first-party workflows.
    #[test]
    fn every_builtin_template_carries_the_maintenance_contract() {
        let prefix = report_contract_prefix_for_workflow_template();
        for (name, report) in [
            ("issue-development", issue_development_report()),
            ("small-change", small_change_report()),
            ("investigation", investigation_report()),
        ] {
            assert!(
                report.body.starts_with(prefix),
                "{name} must lead with the closed contract prefix"
            );
            let slices = calm_types::report_blocks::split_body(&report.body);
            assert!(
                slices[0].raw.ends_with("-->\n\n"),
                "{name}: the contract must be its own closed block, got {:?}",
                slices[0].raw
            );
            assert_eq!(
                report.body.matches("-->").count(),
                1,
                "{name}: a second `-->` would close the comment early and leak the tail"
            );
            assert!(
                report.body.contains("# Plan"),
                "{name} keeps its own plan section"
            );
            assert!(
                report.body.contains("Plan —— 预置计划"),
                "{name} must say what happens to `# Plan` once its tasks are activated"
            );
            assert!(
                report.report_startup_read_required(),
                "{name} is not the default skeleton"
            );
        }
    }

    /// #1185 §1.5 B — the anti-flattening rewrite of the three intros.
    ///
    /// Activation targets `task` blocks only. Ordering the agent to "replace
    /// the prose blocks" is precisely the instruction that destroys a report
    /// arriving with its own structure — and the templates are the delivery
    /// path the contract mechanism exists for, so an intro still carrying it
    /// would put two contradictory orders in one document.
    #[test]
    fn no_builtin_intro_orders_the_prose_replaced() {
        for (name, report) in [
            ("issue-development", issue_development_report()),
            ("small-change", small_change_report()),
            ("investigation", investigation_report()),
        ] {
            let intro = &report.body;
            assert!(
                !intro.contains("Treat the prose"),
                "{name} must not treat prose blocks as a plan to activate"
            );
            assert!(
                !intro.contains("the prose and"),
                "{name} must not scope activation to prose blocks"
            );
            assert!(
                intro.contains("Prose blocks are NOT a plan to activate"),
                "{name} must say prose is maintained, not replaced"
            );
            assert!(
                intro.contains("task blocks"),
                "{name} must scope activation to task blocks"
            );
        }
    }

    #[test]
    fn known_keys_round_trip() {
        for key in WORKFLOW_TEMPLATE_KEYS {
            assert!(is_workflow_template_key(key));
            assert!(
                WORKFLOW_TEMPLATES
                    .iter()
                    .any(|template| template.key == key)
            );
            assert!(workflow_template_report(key).is_some());
        }
        assert!(!is_workflow_template_key("missing-workflow"));
        assert!(workflow_template_report("missing-workflow").is_none());
    }
}
