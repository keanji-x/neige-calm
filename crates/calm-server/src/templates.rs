//! #1110 S6 — kernel-seeded template reports.
//!
//! Three system-cove template waves hold the former git-forge plan as
//! report `task` blocks. `POST /api/waves` with a matching `template_id`
//! forks that report. Overlay payload `{schemaVersion:1, template_key}`
//! is the stable lookup.

use crate::mcp_server::tools::plan::{PlanTaskInput, plan_template_task_block_payload};
use crate::wave_report::WaveReportPayload;
use calm_types::report_blocks::{KIND_TASK, parse_fence, render_fence, split_body};
use calm_types::wave_report::report_contract_prefix_for_template;
use serde_json::{Value, json};

pub const ISSUE_DEVELOPMENT: &str = "issue-development";
pub const SMALL_CHANGE: &str = "small-change";
pub const INVESTIGATION: &str = "investigation";

pub struct Template {
    pub key: &'static str,
    pub title: &'static str,
}

/// The template roster. `static`, not `const`, so [`template_by_key`] can
/// hand out `&'static` borrows into it instead of into a per-use temporary.
pub static TEMPLATES: [Template; 3] = [
    Template {
        key: ISSUE_DEVELOPMENT,
        title: "Issue development",
    },
    Template {
        key: SMALL_CHANGE,
        title: "Small change",
    },
    Template {
        key: INVESTIGATION,
        title: "Investigation",
    },
];

/// #1209 — the roster's single fallible lookup: "is this id a template, and
/// if so which one". `POST /api/waves` admits an id iff this returns `Some`.
///
/// It derives from [`TEMPLATES`] rather than from a second array of
/// keys, so "the list the picker shows" and "the set create accepts" cannot
/// drift: there is nothing to keep in sync. The second roster that used to
/// exist — a key-array constant plus the predicate that walked it — was
/// exactly that duplication and is gone with this slice.
///
/// This is not the *only* place the roster is read — `list_wave_templates`
/// iterates `TEMPLATES` directly, and so does the seeding loop. It is
/// the only place that answers "is this arbitrary caller-supplied string one of
/// them", which is the question `:779`'s deleted special case used to answer
/// twice.
pub fn template_by_key(key: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|template| template.key == key)
}

pub fn template_report(key: &str) -> Option<WaveReportPayload> {
    match key {
        ISSUE_DEVELOPMENT => Some(issue_development_report()),
        SMALL_CHANGE => Some(small_change_report()),
        INVESTIGATION => Some(investigation_report()),
        _ => None,
    }
}

/// The typed task list a template's constants declare, **for tests only**.
///
/// #1209 had `GET /api/wave-templates` read the picker's task list through this
/// function. #1230 moved the production read onto
/// [`template_task_payloads`], which returns whole task-block payloads
/// rather than a struct that models only some of the vocabulary — see
/// [`template_task_payloads_from_body`] for why that distinction is
/// load-bearing. What is left here is the authored source the tests compare
/// against, so `the_picker_projection_matches_the_constant_task_list` is
/// checking the payload path against a hand-written list and not against
/// itself.
#[cfg(test)]
pub fn template_tasks(key: &str) -> Option<Vec<PlanTaskInput>> {
    match key {
        ISSUE_DEVELOPMENT => Some(issue_development_tasks()),
        SMALL_CHANGE => Some(small_change_tasks()),
        INVESTIGATION => Some(investigation_tasks()),
        _ => None,
    }
}

/// Read the task blocks back out of a rendered template report body, **as the
/// payloads they are**.
///
/// ## Why this returns `Value` and not `PlanTaskInput`
///
/// The first cut deserialized each payload into [`PlanTaskInput`]. That was a
/// silent data-loss bug, not a typing preference: `PlanTaskInput` is
/// `#[serde(deny_unknown_fields)]`, and `refs`, `released_by_user`,
/// `tombstone`, `tombstoned_by` and `spawn` are all first-class task-block
/// vocabulary (`report_blocks::kinds`) that it does not carry. A **well-formed**
/// task fence using any of them failed to deserialize and was dropped by the
/// "lenient" filter — and the surviving list then drove a whole-document
/// rewrite. Two consequences, both reproduced before this was changed:
///
/// * a task carrying `refs` vanished from `GET /api/wave-templates` (the exact
///   drift #1230 exists to remove) and made the template permanently unsavable,
///   because the rewrite dropped a live task block and
///   `guard_task_declarations` refuses that;
/// * a **tombstone** was erased by a save that only changed the title, silently
///   reversing a #1179-governed deletion — the guard cannot catch it, its
///   removal check is gated on `!is_tombstone(old)`.
///
/// Keeping the payload whole removes the failure mode rather than patching it:
/// there is no "unknown field" to lose, and the round trip is an identity on
/// everything this module does not deliberately restamp. Nothing here needs the
/// typed struct — the picker reads `key` and `goal`, and everything else is
/// carried whole.
///
/// #1300 S1 deleted the Settings editor this paragraph used to name as the
/// consumer. The reason to keep the payload whole did not go with it: the read
/// still feeds `POST /api/waves`, so a field this function dropped would be a
/// field an instantiated wave never receives.
///
/// Still lenient in the one way `split_body` is: a slice that is not a
/// well-formed `task` fence — prose, another kind, unparseable JSON — is
/// skipped. That is leniency about *shape*, which the parser has already
/// decided, not about vocabulary.
pub fn template_task_payloads_from_body(body: &str) -> Vec<Value> {
    split_body(body)
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == KIND_TASK)
        .map(|fence| fence.payload)
        .collect()
}

/// The editable projection of one task payload: `key` and `goal`, or `None` for
/// a payload that has neither (a tombstone).
///
/// Used by the read side to answer "what tasks does this template pre-set" for
/// the New wave picker. Tombstones are *not* tasks the picker should advertise,
/// but they must still survive the read untouched — which is why the
/// filtering happens here, at the projection, and never in
/// [`template_task_payloads_from_body`].
pub fn task_payload_key_and_goal(payload: &Value) -> Option<(String, String)> {
    if payload
        .get("tombstone")
        .is_some_and(|value| !value.is_null())
    {
        return None;
    }
    let key = payload.get("key")?.as_str()?.to_string();
    let goal = payload.get("goal")?.as_str()?.to_string();
    Some((key, goal))
}

/// The task payloads a template's built-in constants render to, for the
/// not-yet-seeded case. Same shape the body would yield, so the read side has
/// one type on both branches.
pub fn template_task_payloads(key: &str) -> Option<Vec<Value>> {
    let body = template_report(key)?.body;
    Some(template_task_payloads_from_body(&body))
}

/// Placeholder so `require_task_gates` does not treat these as scheduled
/// work. Spec must replace the block with a real `gate` from the target
/// repo before setting `ready: true`. Never an executed shell command.
pub const AUTHOR_REAL_GATE: &str = "author a real gate from the target repo toolchain \
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
    let mut body = report_contract_prefix_for_template().to_string();
    body.push_str(intro.trim_end());
    body.push_str("\n\n");
    for task in tasks {
        let mut payload = plan_template_task_block_payload(task);
        // #1300 — declared here as `spec`, which is what an instantiated wave
        // ends up with either way. Before #1300 this said `user` and the fork
        // step rewrote it one instruction later; the `user` was not a claim
        // about authorship but a consequence of the seeding write going through
        // `persist_report` as `EditAuthor::User`, and `guard_task_declarations`
        // requiring a new task block's `declared_by` to match its author.
        //
        // Instantiation no longer goes through `persist_report` at all
        // (`routes::waves::prepare_template_report`), so nothing constrains
        // this to the author of a write that does not happen. `spec` is the
        // honest value: a recipe's tasks are pre-set, not user-declared, and
        // they stay `ready: false` until the normal Spec/user flow releases
        // them.
        payload["ready"] = json!(false);
        payload["declared_by"] = json!("spec");
        body.push_str(&render_fence("task", &payload));
        body.push('\n');
    }
    WaveReportPayload::new(summary, body)
}

fn issue_development_report() -> WaveReportPayload {
    report_from_tasks(
        "Issue development",
        ISSUE_DEVELOPMENT_INTRO,
        &issue_development_tasks(),
    )
}

fn issue_development_tasks() -> Vec<PlanTaskInput> {
    vec![
        task(
            "inspect-issue",
            "Read the bound template input, view the source issue via gh.issue.view, and cross-check input.repo against the git remote of the wave cwd.",
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
    ]
}

fn small_change_report() -> WaveReportPayload {
    report_from_tasks("Small change", SMALL_CHANGE_INTRO, &small_change_tasks())
}

const SMALL_CHANGE_INTRO: &str = concat!(
    "# Plan\n\n",
    "Short inspect → implement → verify loop. Treat these task blocks as ",
    "the authoritative pre-set plan. Activate by replacing those task blocks, ",
    "authoring a real `gate` from the target repo toolchain (formatter, linter, ",
    "tests), and setting `ready: true`. Do not mint duplicate tasks. Prose blocks ",
    "are NOT a plan to activate: maintain them per this document's own contract.\n"
);

fn small_change_tasks() -> Vec<PlanTaskInput> {
    vec![
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
    ]
}

fn investigation_report() -> WaveReportPayload {
    report_from_tasks("Investigation", INVESTIGATION_INTRO, &investigation_tasks())
}

const INVESTIGATION_INTRO: &str = concat!(
    "# Plan\n\n",
    "Read-only investigation. Gather facts, then write findings in this ",
    "report. Do not open a pull request, merge, or otherwise change the ",
    "bound repository. Treat these task blocks as the authoritative ",
    "pre-set plan. Activate by replacing those task blocks with `ready: true`. ",
    "Do not mint duplicate tasks. Prose blocks are NOT a plan to activate: ",
    "maintain them per this document's own contract.\n"
);

fn investigation_tasks() -> Vec<PlanTaskInput> {
    vec![
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
    ]
}

/// Pre-S5 git-forge `spec_instructions`, adapted off the deleted prompt
/// sections (`## Bound Template Input` / `## Bound Template Gates`).
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

Template input: the wave's bound `template_input` JSON is the task's \
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
    use calm_types::report_blocks::{KIND_TASK, parse_fence, split_body};
    use std::collections::BTreeSet;

    /// #1230 — reading a task block out of a body and rendering it back must be
    /// an **identity on the payload**, not merely agree on the fields some
    /// struct happens to model. The first cut deserialized into
    /// `PlanTaskInput` (`deny_unknown_fields`) and silently dropped any block
    /// carrying `refs` / `released_by_user` / `tombstone`; asserting identity is
    /// what makes that class impossible rather than fixed for the fields we
    /// happened to think of.
    ///
    /// The whole-document version of this property — that a *save* preserves
    /// every block and its id — is an integration test
    /// (`a_save_preserves_blocks_it_does_not_edit`), because it is about the
    /// report's blocks and not about this module's constants.
    #[test]
    fn parsing_a_task_fence_and_rendering_it_back_is_an_identity() {
        for key in TEMPLATES.iter().map(|template| template.key) {
            let body = template_report(key).expect("known key").body;
            let payloads = template_task_payloads_from_body(&body);
            assert!(!payloads.is_empty(), "{key}: no task payloads parsed");
            for payload in &payloads {
                let fence = render_fence(KIND_TASK, payload);
                assert!(
                    body.contains(&fence),
                    "{key}: re-rendering a parsed payload did not reproduce its fence:\n{fence}"
                );
            }
        }
    }

    /// The picker projection still sees exactly the constants' keys and goals.
    #[test]
    fn the_picker_projection_matches_the_constant_task_list() {
        for key in TEMPLATES.iter().map(|template| template.key) {
            let expected: Vec<(String, String)> = template_tasks(key)
                .expect("known key")
                .into_iter()
                .map(|task| (task.key, task.goal))
                .collect();
            let projected: Vec<(String, String)> =
                template_task_payloads_from_body(&template_report(key).unwrap().body)
                    .iter()
                    .filter_map(task_payload_key_and_goal)
                    .collect();
            assert_eq!(projected, expected, "{key}");
        }
    }

    /// Prose the user added through the ordinary wave report editor is not a
    /// task and must not be read as one — the lenient-read claim in the
    /// function's doc, exercised rather than asserted.
    #[test]
    fn body_prose_and_foreign_fences_are_skipped_not_parsed() {
        let mut body = template_report(SMALL_CHANGE).expect("known key").body;
        let before = template_task_payloads_from_body(&body).len();
        body.push_str("\n## Notes\n\nSomething the user typed.\n\n");
        body.push_str("```neige-block table\n{\n  \"rows\": []\n}\n```\n");
        body.push_str("```neige-block task\nnot json\n```\n");
        assert_eq!(template_task_payloads_from_body(&body).len(), before);
    }

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
    /// guardrails would vanish on exactly the first-party templates.
    #[test]
    fn every_builtin_template_carries_the_maintenance_contract() {
        let prefix = report_contract_prefix_for_template();
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
        for template in &TEMPLATES {
            let key = template.key;
            assert!(template_by_key(key).is_some());
            assert_eq!(template_by_key(key).map(|found| found.key), Some(key));
            assert!(template_report(key).is_some());
            assert!(template_tasks(key).is_some());
        }
        assert!(template_by_key("missing-template").is_none());
        assert!(template_report("missing-template").is_none());
        assert!(template_tasks("missing-template").is_none());
    }

    /// #1209 — the picker's tooltip lists a template's pre-set tasks, and it
    /// reads them from `template_tasks`. That is only honest while the
    /// list is the *same* slice the report renders: a task added to the report
    /// but not to the list (or a list entry that seeds nothing) would make the
    /// tooltip promise something the forked report does not contain.
    ///
    /// ## What this can and cannot catch
    ///
    /// Today it is **a construction guard, not a drift detector**: every
    /// `*_report()` in this module is built by `report_from_tasks` from the
    /// matching `*_tasks()` — the same `Vec`, one call apart — so a divergence
    /// is not expressible and this test is green by construction. What it
    /// guards is the *next* edit: a `*_report()` that stops taking its blocks
    /// from its own `*_tasks()` (hand-written fences, an extra block appended,
    /// a task quietly dropped from the list) fails here immediately.
    ///
    /// The drift that actually reaches the picker — the route serving a
    /// different list than the one seeded — is out of this module's reach and
    /// is pinned in
    /// `tests/cases/wave_templates_read.rs::every_template_lists_the_tasks_its_report_pre_sets`,
    /// which asserts the HTTP response's keys in order.
    ///
    /// Both directions are real, and neither is a substring count: forward,
    /// every listed key/goal is in a `task` fence; backward, the fences the
    /// body actually parses to are read out and their key **set** compared,
    /// with duplicates rejected. Counting `"key": "` occurrences would have
    /// been the fragile version — any nested payload carrying that literal
    /// would have made it fail for the wrong reason. Both halves were measured
    /// by mutation, not asserted: appending one extra `task` fence to every
    /// seeded body turns this red on the key set (`ghost` shows up on the right
    /// of the diff), while giving each task a `context` of `{"key": "..."}`
    /// leaves it green and would have taken the old count from 3 to 6 on
    /// `small-change` alone.
    #[test]
    fn listed_tasks_are_exactly_the_report_task_blocks() {
        for template in &TEMPLATES {
            let key = template.key;
            let tasks = template_tasks(key).expect("known key");
            let body = template_report(key).expect("known key").body;
            assert!(!tasks.is_empty(), "{key} lists no tasks");

            // The report's own reader, not a string scan: `split_body` cuts the
            // well-formed fences out and `parse_fence` gives their payloads, so
            // this sees exactly the task blocks a forked wave would.
            let mut seeded: Vec<String> = Vec::new();
            for slice in split_body(&body) {
                let Some(fence) = parse_fence(&slice.raw) else {
                    continue;
                };
                if fence.kind != KIND_TASK {
                    continue;
                }
                seeded.push(
                    fence.payload["key"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{key}: task block without a string key"))
                        .to_string(),
                );
            }

            let mut unique = seeded.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                unique.len(),
                seeded.len(),
                "{key}: the seeded report declares the same task key twice: {seeded:?}"
            );

            let listed: BTreeSet<&str> = tasks.iter().map(|task| task.key.as_str()).collect();
            let fenced: BTreeSet<&str> = seeded.iter().map(String::as_str).collect();
            assert_eq!(
                listed, fenced,
                "{key}: the advertised task keys and the seeded task blocks differ"
            );

            for task in &tasks {
                assert!(
                    body.contains(&task.goal),
                    "{key}: listed goal for {} is not the seeded goal",
                    task.key
                );
            }
        }
    }
}

#[cfg(test)]
mod repro_1239 {
    use super::*;

    /// Channel-B finding, reproduced before any fix.
    ///
    /// `PlanTaskInput` is `#[serde(deny_unknown_fields)]`, and `refs` /
    /// `released_by_user` / `tombstone` / `tombstoned_by` are all first-class
    /// task-block vocabulary it does not carry. A *well-formed* task fence
    /// using any of them therefore fails to deserialize and is dropped by the
    /// lenient filter — which is not leniency, it is silent data loss feeding a
    /// whole-document rewrite.
    #[test]
    fn a_wellformed_task_fence_with_task_block_vocabulary_is_silently_dropped() {
        let mut body = template_report(SMALL_CHANGE).expect("known key").body;
        body.push_str(&render_fence(
            KIND_TASK,
            &json!({
                "key": "with-refs",
                "kind": "codex",
                "goal": "A task that references a document.",
                "refs": ["neige://wave/w1#b_0001"],
                "ready": false,
                "declared_by": "user",
            }),
        ));
        let parsed = template_task_payloads_from_body(&body);
        let keys: Vec<&str> = parsed
            .iter()
            .filter_map(|p| p.get("key").and_then(Value::as_str))
            .collect();
        assert!(
            keys.contains(&"with-refs"),
            "a well-formed task fence carrying `refs` must survive the read; got {keys:?}"
        );
    }

    /// The same drop applied to a tombstone silently reverses a #1179-governed
    /// deletion: the guard's removal check is gated on `!is_tombstone(old)`, so
    /// nothing stops the rewrite from erasing it.
    #[test]
    fn a_task_tombstone_is_not_erased_by_the_read() {
        let mut body = template_report(INVESTIGATION).expect("known key").body;
        body.push_str(&render_fence(
            KIND_TASK,
            &json!({
                "key": "retired",
                "tombstone": { "reason": null },
                "declared_by": "user",
                "tombstoned_by": "user",
            }),
        ));
        let parsed = template_task_payloads_from_body(&body);
        let keys: Vec<&str> = parsed
            .iter()
            .filter_map(|p| p.get("key").and_then(Value::as_str))
            .collect();
        assert!(
            keys.contains(&"retired"),
            "a tombstone must survive the read; got {keys:?}"
        );
    }
}
