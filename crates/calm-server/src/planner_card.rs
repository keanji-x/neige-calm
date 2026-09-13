//! Planner-card binding (PR6 of #136).
//!
//! Every track gets a single auto-minted **planner card** at create-time. The
//! planner card is the track's "AI authority": the only card whose `AiPlanner`
//! actor is allowed to emit `Event::TrackUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the track's goal + acceptance criteria.
//!
//! This module owns the role-specific prompts and Codex environment
//! construction:
//!
//!   1. [`PLANNER_SYSTEM_PROMPT_TEMPLATE`] — the system prompt used when
//!      starting the planner card's Codex thread. Its prose is data in
//!      `prompts/planner.md` (#1635); this module only embeds it and
//!      substitutes the per-spawn placeholders.
//!
//! Atomicity story for the planner card itself lives in
//! `routes::tracks::create_track` — the planner card row and both
//! `Event::TrackUpdated` / `Event::CardAdded` envelopes are produced in a
//! single `write_with_events_typed` transaction.

/// The planner-agent system prompt template. The prose is data, not code:
/// it lives in `prompts/planner.md` (issue #1635 S1a) and is embedded here
/// byte-for-byte so the binary needs no file at runtime.
///
/// Placeholders substituted by [`render_system_prompt`]:
///
/// * `{track_id}`: when the Codex thread starts, the kernel replaces it with
///   the freshly minted track id so the agent has a stable reference for the
///   `calm.*` track-state / report tools.
/// * `{planner_wake_authors}`: rendered from
///   [`crate::dispatcher::PLANNER_WAKE_AUTHORS`], the dispatcher's own wake
///   set for `track.report_edited`. Rendered rather than hand-written so
///   editing the dispatch rule rewrites the prompt in the same commit.
///
/// Wording is pinned by the whole-document golden
/// `tests/goldens/issue_development_planner_prompt.txt` (regenerate with
/// `REGEN_PLANNER_PROMPT_GOLDEN=1`, then hand-verify the diff). The
/// code-relation tests — what the prompt must agree with elsewhere in the
/// code (tool registry, dispatcher wake set, task kinds, birth skeleton) —
/// live in `mod tests` below.
pub(crate) const PLANNER_SYSTEM_PROMPT_TEMPLATE: &str = include_str!("../prompts/planner.md");

/// Head of the **claude** (CLI-completion) worker prompt — everything
/// before the shared `## Reading track state` tail. Step 3 reports through
/// the `neige` shell CLI. A literal-yielding macro so it can be
/// `concat!`'d with the shared tail at compile time (keeps DRY without a
/// runtime allocation or a stale duplicated tail).
macro_rules! worker_prompt_head_cli {
    () => {
        "\
You are a worker agent under planner card on track `{track_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the track's shape before \
   starting — but don't poll it; the track snapshot you receive once is \
   enough.
2. Execute the task. Make tool calls, write files, run commands \
   — whatever the goal requires.
3. When the task is done, report exactly once via the `neige` shell CLI:
   * On success: `neige task-completed --idempotency-key K --result <json-or-text>` \
     where `K` echoes the idempotency key the kernel handed you. \
     Append `--artifact <path>` (may repeat) for any file/blob references \
     you produced.
   * On failure: `neige task-failed --idempotency-key K --reason '<text>'` \
     with a free-form failure description.
4. Exit. You are short-lived by design — run your single job and stop. \
   Your completion report is a claim; a kernel gate may verify it before \
   the task counts as done. The kernel delivers ungated reports, failures, \
   or gate results to the planner card as pushed turn inputs, and the planner \
   continues the track from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a planner-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers; \
`calm.task.dispatch` is Planner-only, and the kernel's role gate (#583) still \
refuses worker-actor dispatch emits from old paths. If the job needs \
further decomposition, report `task.failed` with a reason \
explaining what's missing and the planner will handle re-decomposition.

"
    };
}

/// Head of the **codex** (MCP-completion) worker prompt — everything
/// before the shared `## Reading track state` tail. Step 3 reports through
/// the native `calm.task.complete` / `calm.task.fail` MCP tools.
macro_rules! worker_prompt_head_mcp {
    () => {
        "\
You are a worker agent under planner card on track `{track_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the track's shape before \
   starting — but don't poll it; the track snapshot you receive once is \
   enough.
2. Execute the task. Make tool calls, write files, run commands \
   — whatever the goal requires.
3. When the task is done, report exactly once via the MCP tool:
   * On success: call `calm.task.complete` with `idempotency_key` = K \
     (the kernel task id you were handed). Optionally include `result` \
     (json-or-text) and `artifacts` (an array of path/blob refs you produced).
   * On failure: call `calm.task.fail` with `idempotency_key` = K and a \
     free-form `reason` (required).
4. Exit. You are short-lived by design — run your single job and stop. \
   Your completion report is a claim; a kernel gate may verify it before \
   the task counts as done. The kernel delivers ungated reports, failures, \
   or gate results to the planner card as pushed turn inputs, and the planner \
   continues the track from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a planner-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers; \
`calm.task.dispatch` is Planner-only, and the kernel's role gate (#583) still \
refuses worker-actor dispatch emits from old paths. If the job needs \
further decomposition, report `task.failed` with a reason \
explaining what's missing and the planner will handle re-decomposition.

"
    };
}

/// Shared `## Reading track state` tail — concatenated into BOTH worker
/// prompts. Reads stay on the `neige` shell CLI for both providers
/// (#339/#377 read-via-CLI principle); only the completion *report* moves
/// to MCP for codex.
macro_rules! worker_prompt_tail {
    () => {
        "\
## Reading track state

You may read your track's state READ-ONLY from the shell with the `neige` \
CLI: `neige state` reads the track shape, `neige ls [path]` lists views, \
and `neige cat <path>` reads one view. Useful paths include `/`, \
`runs/index.json`, \
`runs/<idempotency_key>.md`, `runs/<idempotency_key>.json`, \
`cards/<card_id>/.payload.json`, and `cards/<card_id>/runtime.json`. \
`.payload.json` is the card's own payload; runtime identity/status lives \
in `runtime.json`. These views are own-track-only; cross-track reads are forbidden.
"
    };
}

/// Worker-agent system prompt. PR8 (#136) replaces the PR6 stub with
/// the production prompt: workers are short-lived, fire-and-forget,
/// driven by the kernel scheduler from the planner-maintained plan. They
/// run one job and exit.
///
/// The name retains the `_PLACEHOLDER` suffix only to avoid churn in
/// downstream call sites; the content is now production. A followup
/// can rename this to `WORKER_SYSTEM_PROMPT_TEMPLATE` for symmetry
/// with [`PLANNER_SYSTEM_PROMPT_TEMPLATE`] when there's no other PR
/// touching this file.
///
/// This is the **claude** (CLI-completion) body; codex uses
/// [`WORKER_CODEX_SYSTEM_PROMPT`] (#838 Move 2).
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str =
    concat!(worker_prompt_head_cli!(), worker_prompt_tail!());

/// codex worker variant (#838 Move 2). Identical to
/// [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] except step 3: completion is
/// reported through the native `calm.task.complete` / `calm.task.fail`
/// MCP tools (channel 2 — DaemonTrust + codex-injected `_meta.threadId`)
/// instead of the `neige` shell CLI. This decouples the kernel-critical
/// completion path from the per-thread `shell_environment_policy` env
/// (channel 3) that keeps getting silently dropped (#738/#747/#836).
///
/// claude keeps [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] (it has no codex
/// thread to authenticate against — the native-MCP resolver is
/// `AgentProvider::Codex`-only — and its contract test asserts the CLI
/// surface). The shared `## Reading track state` block (`worker_prompt_tail!`)
/// is concatenated into both, keeping reads on the CLI for both providers.
pub(crate) const WORKER_CODEX_SYSTEM_PROMPT: &str =
    concat!(worker_prompt_head_mcp!(), worker_prompt_tail!());

/// The tool surface and the marker protocol shared by **both** assistant
/// identities.
///
/// A macro rather than a `const` so the two prompts can be built with
/// `concat!` and stay `&'static str`, the same shape `worker_prompt_head_mcp!`
/// uses. #1343 forks the assistant's *identity* — first duty, and who owns the
/// document — and nothing else; keeping the mechanics in one place is what
/// stops the halves that are not in dispute from drifting.
macro_rules! assistant_prompt_mechanics {
    () => {
        "
## What you can do

* **Read the report.** Use `calm.report.read` for the track report. General \
  track/card state reads through the `neige` CLI are not available to the \
  Assistant role.
* **Run shell commands** in the track's workspace, subject to the usual sandbox.
* **Write prose into the track report** through the block tools: \
  `calm.report.blocks.upsert`, `.move`, `.delete` \
  (`calm.report.blocks.kinds` lists the block vocabulary), or \
  `calm.report.write_markdown` for a whole-document rewrite.

## What you cannot do

Lifecycle transitions, plan writes, task verdicts, review, admin, and the \
whole-document `calm.report.write` are not yours. Neither are `task` blocks: \
the track's plan belongs to the planner agent, and a `task` block written from here \
is rejected — the whole write, not just that block. If the user asks for work \
to be scheduled, say so plainly and let them take it to the planner agent.

## Loading deferred tools

Codex may defer MCP tools until they are requested. Before report work, use \
tool search to load the exact `calm.report.read` tool and the exact report write \
tool you need. If a named tool is not immediately visible, use tool search to \
load that exact `calm.*` tool; do not substitute a planner-only tool or declare \
the report tools unavailable merely because they are deferred.

## Writing to the report, concretely

1. Call `calm.report.read` with `with_markers: true` FIRST. It gives you the \
   document's `docRev` and every block's `{id, kind, rev}`.
2. To add a block, pass that `docRev` as `if_doc_rev`. To replace one, pass \
   the block's own `rev` as `if_rev` together with its `id`.
3. A prose block's `markdown` is the WHOLE block, not only the new paragraph. \
   When replacing a headed section, keep its `#` / `##` heading and trailing \
   newline; omitting them destroys the block boundary and can join the next section.
4. `calm.report.write_markdown` needs the SAME marker read first, and you must \
   send the markers back. Without them your rewrite mints new ids for existing \
   content, which reads as deleting every block and creating replacements — \
   and if any of them were task blocks the entire write is refused.
5. Another session may be writing at the same time. A revision conflict means \
   somebody else moved first: re-read and reapply, do not retry blindly.
"
    };
}

/// #1189 — the track assistant's system prompt.
///
/// Deliberately not a trimmed copy of [`PLANNER_SYSTEM_PROMPT_TEMPLATE`]: most of
/// that prompt instructs the agent to drive the lifecycle state machine and the
/// plan, and every one of those tools rejects `CardRole::Assistant` at the
/// handler. Describing them here would teach the agent to spend turns on calls
/// that can only come back `-32602`.
///
/// Two things in here are load-bearing rather than stylistic:
///
/// * **"read with markers before you rewrite"** — a `calm.report.write` style
///   full-document rewrite is unavailable to this role, and a block write that
///   re-mints ids reads as "delete every task block and create new ones", which
///   the task-block guard rejects as a whole transaction (design §3.2a-bis.4).
///   The marker read is what keeps existing block ids stable.
/// * **"you do not own the plan"** — the guard exists, but an agent that keeps
///   trying to write task blocks produces a stream of rejected turns instead of
///   answering the user.
pub(crate) const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    "\
You are an assistant conversation on track `{track_id}`.

You are talking with the user. Answer them. You are NOT the track's planner agent: \
you do not own the track's lifecycle, its plan, or its workers, and the kernel \
will reject you if you try to drive any of them.
",
    assistant_prompt_mechanics!(),
    // "A guest" is correct HERE: an ordinary track's report is maintained by
    // that track's planner agent. It is false on the launchpad, which is why
    // #1343 gave that track its own closing paragraph instead of editing this
    // one.
    "
Keep the report's own structure and conventions; you are a guest in a document \
the planner agent maintains.
",
);

/// #1343 — the assistant on **Today's launchpad track**.
///
/// Same tools, same marker protocol, different job. Measured on the 4140
/// preview: told explicitly to write a block, the agent wrote one (`docRev`
/// 1→2), so the tool surface, the CAS handshake and the write permission were
/// all already working. Told casually what had happened, it made zero tool
/// calls and answered in chat. The prompt was the cause, in two places:
///
/// * the first duty was **"You are talking with the user. Answer them."**, with
///   writing the report listed under *What you can do* — a capability, not a
///   duty, so chatting was the default path;
/// * the closing sentence said the agent is **a guest in a document the planner
///   agent maintains**. On an ordinary track that is true. On the launchpad
///   there is no planner agent writing today's report — by design this
///   conversation is the writer — so the prompt was telling it the document was
///   not its to touch.
///
/// This template inverts both and leaves the mechanics identical. It changes
/// nothing for any other track: the fork is selected by
/// [`routes::today::is_launchpad_track`] at `thread/start`, the one criterion
/// the activity briefing also uses.
///
/// **`developer_instructions` are handed over at thread start**, so a
/// conversation that already exists keeps the identity it was started with. A
/// new conversation is what picks this up.
///
/// [`routes::today::is_launchpad_track`]: crate::routes::today::is_launchpad_track
pub(crate) const LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    "\
You are the writer of today's progress report, on Today's launchpad track \
`{track_id}`.

Your first duty is to keep that report current. The report is yours: no planner \
agent maintains it, and if you do not record the day, nothing else will. \
Talking with the user is how you find out what to record — it is not the job \
itself.

You are NOT a planner agent: you do not own any track's lifecycle, its plan, or \
its workers, and the kernel will reject you if you try to drive any of them.
",
    assistant_prompt_mechanics!(),
    "
When the user tells you what happened, what to note down, or what to change, \
write it into the report and then confirm briefly in the chat. Answering in \
chat while leaving the report untouched is the one failure mode to avoid: the \
conversation is not where the day is kept.

The report body opens with a maintenance contract in an HTML comment. Follow \
it — its section list, its rewrite-don't-append rule and its length budget are \
the report's structure — and read whatever it says about another agent filling \
a section as addressed to you.
",
);

/// Render the report-edit authors that wake the planner, straight from the
/// dispatcher's wake set, in the wire spelling the `track.report_edited`
/// payload actually carries (so the prompt names what the agent will see).
fn planner_wake_authors_prose() -> String {
    crate::dispatcher::PLANNER_WAKE_AUTHORS
        .iter()
        .map(|author| format!("`{}`", author.wire_str()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Substitute the per-spawn placeholders into a prompt template:
/// `{track_id}` and `{planner_wake_authors}`. Lifted out as its own helper so
/// call sites do not need rewriting when the substitution set grows.
pub(crate) fn render_system_prompt(template: &str, track_id: &str) -> String {
    template
        .replace("{track_id}", track_id)
        .replace("{planner_wake_authors}", &planner_wake_authors_prose())
}

#[cfg(test)]
const TASK_BLOCK_PROTOCOL_GOLDEN: &str = concat!(
    "   * Maintain task declarations as report `task` blocks. Read the report with ",
    "`calm.report.read`; for create, pass its `docRev` as `if_doc_rev`, while ",
    "replace passes the target block's `rev` as `if_rev`. Use ",
    "`calm.report.blocks.upsert` for both operations. To start an authorized Planner task, ",
    "its payload needs a per-track-unique ",
    "`key`, `kind` (`codex`, `claude`, or `terminal`), `ready: true`, ",
    "and `declared_by: \"spec\"`; it may also carry `acceptance`, `depends_on` ",
    "sibling keys, `priority`, and usually `gate`. Use `calm.plan.cancel` to ",
    "cancel a pending projected task. Use `calm.plan.list` to inspect status. ",
    "A `codex`/`claude` task requires `goal`, a natural-language objective, and ",
    "forbids `command`. A `terminal` task requires `command`, the exact Shell ",
    "command passed verbatim to `/bin/sh -c`, and forbids `goal`."
);

/// Exact paragraph oracle for the static task-block protocol. The shipped
/// template's fully rendered prompt has a separate whole-document golden;
/// free-text contradictions cannot be proved absent with a keyword list.
#[cfg(test)]
pub(crate) fn validate_planner_prompt_contract(prompt: &str) -> Result<(), String> {
    let start = prompt
        .find("   * Maintain task declarations as report `task` blocks.")
        .ok_or_else(|| "task-block protocol paragraph is missing".to_string())?;
    let remainder = &prompt[start..];
    let end = remainder
        .find("\n   * Every codex or claude task")
        .ok_or_else(|| "task-block protocol paragraph terminator is missing".to_string())?;
    let actual = &remainder[..end];
    if actual != TASK_BLOCK_PROTOCOL_GOLDEN {
        return Err(format!(
            "task-block protocol differs from golden\nexpected: {TASK_BLOCK_PROTOCOL_GOLDEN:?}\nactual:   {actual:?}"
        ));
    }

    Ok(())
}

/// Test-only seam (#838 A1 e2e): render the rendered worker prompt for the
/// provider under test. `codex=true` yields the native-MCP-completion body
/// ([`WORKER_CODEX_SYSTEM_PROMPT`], what `codex_adapter` ships);
/// `codex=false` yields the CLI body ([`WORKER_SYSTEM_PROMPT_PLACEHOLDER`],
/// what `claude_adapter` ships and the RED baseline). Doc-hidden so it does
/// not widen the public prompt API beyond the e2e harness.
#[doc(hidden)]
pub fn render_worker_prompt_for_e2e(track_id: &str, codex: bool) -> String {
    let role = if codex {
        SeededCardRole::WorkerCodex
    } else {
        SeededCardRole::Worker
    };
    render_system_prompt(role.prompt_template(), track_id)
}

/// Test-only seam (#1189): the exact `developer_instructions` string a track
/// assistant's `thread/start` must carry.
///
/// Exposed rather than re-spelled in the test on purpose. An integration test
/// that asserted on a substring ("contains `assistant`") would stay green if the
/// assistant profile were wired to the PLANNER prompt, which is one of the two
/// mutations #1189's A2 gate has to catch; a test that re-declared the template
/// would stay green if the adapter stopped rendering the placeholder. Handing
/// out the rendered string makes the assertion an equality against production's
/// own value.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_assistant_prompt_for_test(track_id: &str) -> String {
    render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, track_id)
}

/// #1343 — the same seam for the launchpad assistant's identity.
///
/// Its own function rather than a bool parameter on the one above: the
/// adapter's fork picks between two named templates, and a test that passed a
/// flag would be asserting on the flag rather than on which template shipped.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_launchpad_assistant_prompt_for_test(track_id: &str) -> String {
    render_system_prompt(LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE, track_id)
}

/// Roles that legitimately need role-specific Codex setup.
/// Carved out of [`crate::model::CardRole`] so the seeding helper can
/// only ever be handed a value that maps to a system-prompt template
/// (no general Worker path to silently fall through). PR6 followup of
/// issue #136 — note 3 from the original review.
///
/// User-facing Worker cards still flow through `routes::codex_cards`'s
/// simpler seed path (which writes a no-prompt config.toml inline); they
/// must not reach this helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeededCardRole {
    /// Planner card minted by `routes::tracks::create_track`. Gets
    /// [`PLANNER_SYSTEM_PROMPT_TEMPLATE`].
    Planner,
    /// Worker card minted by the dispatcher for a **claude** provider.
    /// Gets [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] — completion is reported
    /// through the `neige` shell CLI (claude has no codex thread for the
    /// native-MCP path and its contract test asserts the CLI surface).
    Worker,
    /// Worker card minted by the dispatcher for a **codex** provider
    /// (#838 Move 2). Gets [`WORKER_CODEX_SYSTEM_PROMPT`] — completion is
    /// reported through the native `calm.task.complete` / `calm.task.fail`
    /// MCP tools, decoupling the kernel-critical completion path from the
    /// channel-3 exec-shell env.
    WorkerCodex,
}

impl SeededCardRole {
    pub(crate) fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Planner => PLANNER_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
            SeededCardRole::WorkerCodex => WORKER_CODEX_SYSTEM_PROMPT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_system_prompt_substitutes_track_id() {
        let out = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-abc");
        assert!(
            out.contains("track `track-abc`"),
            "track id should be substituted; got: {out}"
        );
        assert!(
            !out.contains("{track_id}"),
            "placeholder should be gone; got: {out}"
        );
    }

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        let planner = render_system_prompt(SeededCardRole::Planner.prompt_template(), "track-abc");
        assert!(planner.contains("You are the planner agent for track `track-abc`."));
        assert!(!planner.contains("calm.update_track_state"));
        assert!(!planner.contains("calm.plan.upsert"));
        assert!(planner.contains("calm.report.blocks.upsert"));
        assert!(planner.contains("`ready: true`"));
        assert!(planner.contains("`declared_by: \"spec\"`"));
        assert!(planner.contains("calm.plan.list"));
        assert!(planner.contains("calm.task.dispatch"));
        assert!(planner.contains("calm.task.verdict"));

        let worker = render_system_prompt(SeededCardRole::Worker.prompt_template(), "track-abc");
        assert!(worker.contains("You are a worker agent under planner card on track `track-abc`."));
        assert!(worker.contains("neige task-completed"));
    }

    #[test]
    fn semantic_recovery_waits_for_bound_isolated_briefing_and_keeps_legacy_path() {
        assert!(
            PLANNER_SYSTEM_PROMPT_TEMPLATE.contains("first settlement briefing is still pending")
        );
        assert!(PLANNER_SYSTEM_PROMPT_TEMPLATE.contains("already delivered"));
        assert!(
            PLANNER_SYSTEM_PROMPT_TEMPLATE
                .contains("Legacy/non-isolated failures and threads without Recover")
        );
        assert!(
            PLANNER_SYSTEM_PROMPT_TEMPLATE
                .contains("prefer `Recover(key, reason)` only when the tool is available")
        );
    }

    #[test]
    fn planner_candidate_examples_use_the_native_execution_contract() {
        use calm_types::task_execution::{FileDelivery, IsolatedCodexSelection};
        let prompt =
            crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
                "track-delivery",
                None,
                None,
            );
        for (prefix, producer) in [
            ("Candidate producer context: `", true),
            ("Candidate consumer context: `", false),
        ] {
            let raw = prompt
                .split_once(prefix)
                .unwrap()
                .1
                .split('`')
                .next()
                .unwrap();
            let context: serde_json::Value = serde_json::from_str(raw).unwrap();
            let selection = IsolatedCodexSelection::from_context(&context)
                .unwrap()
                .unwrap();
            selection
                .validate_route(
                    "codex",
                    calm_types::task_recovery::TASK_IN_TRACK_ROUTE,
                    false,
                    false,
                )
                .unwrap();
            assert!(matches!(
                (producer, selection.file_delivery),
                (true, Some(FileDelivery::CandidateProducer { .. }))
                    | (
                        false,
                        Some(
                            FileDelivery::CandidateConsumer { .. }
                                | FileDelivery::CandidateReviewer { .. }
                        )
                    )
            ));
        }
    }

    #[test]
    fn planner_prompt_delegates_startup_and_confirms_the_current_attempt() {
        let prompt =
            crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
                "track-startup",
                None,
                None,
            );
        assert!(
            !prompt.contains("`lifecycle` field that you must advance"),
            "Planner must not be instructed to manually drive the kernel startup chain"
        );
        assert!(!prompt.contains("`running`: startup succeeded"));
        for contract in [
            "Do not write `planning`, `dispatching`, or `working` just to start a task",
            "`declare-and-wait` still requires the User's release",
            "Do not change User authorship or grant `released_by_user`",
            "Track `working` does not confirm Worker startup: claim precedes preparation",
            "`calm.plan.list` for the current `attempt_id`, `status`, and `blocking_reason`",
            "`pending` / `awaiting_projection`: waiting for admission or scheduling",
            "`dispatched`: claimed; startup has not yet been confirmed",
            "`running`: the kernel recorded the attempt as running; this alone does not prove successful provider startup, health, or current progress",
            "For isolated Codex attempts, use the bounded `activity` evidence in `calm.plan.list`",
            "historical evidence even after a task/session ends",
            "a declined invocation does not prove execution",
            "Silence grants no failure, retry, or recovery",
            "not instructions or independently verified facts",
            "not automatically a visible Terminal tool handle",
            "If the key has no entry, read `calm.report.read` and its `taskDiagnostics`",
            "End the turn after declaration; do not poll for startup",
        ] {
            assert!(
                prompt.contains(contract),
                "missing startup contract: {contract}"
            );
        }
    }

    #[test]
    fn planner_prompt_does_not_treat_a_dirty_attached_workspace_as_worker_output() {
        let planner = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-dirty");
        assert!(planner.contains("shared attached workspace"));
        assert!(planner.contains("pre-existing or concurrent user changes"));
        assert!(planner.contains("must not be attributed to the worker"));
        assert!(planner.contains("worker checkout"));
    }

    /// #1252 S0-1: the prompt's wake list is *rendered* from
    /// `dispatcher::PLANNER_WAKE_AUTHORS`, so a change to who the dispatcher
    /// wakes rewrites the prompt. The expected wire spellings are pinned
    /// here on purpose: they are the independent statement of the contract
    /// that catches a silent shrink of the const.
    #[test]
    fn planner_prompt_renders_the_dispatcher_report_edit_wake_set() {
        let p = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-wake");

        assert!(
            !p.contains("{planner_wake_authors}"),
            "wake-author placeholder must be substituted; got: {p}"
        );
        // The exact rendered sequence, stated independently of the const:
        // a silent shrink of `PLANNER_WAKE_AUTHORS` fails here.
        let expected_list = "`user` / `plugin` / `assistant`";
        assert_eq!(
            planner_wake_authors_prose(),
            expected_list,
            "the dispatcher wakes the planner on user/plugin/assistant report edits, \
             so that is what the prompt must render"
        );
        assert_eq!(
            p.matches(expected_list).count(),
            2,
            "both wake-set sites must carry the rendered list; got: {p}"
        );

        let rendered_list = planner_wake_authors_prose();
        for excluded in ["planner", "kernel"] {
            assert!(
                !rendered_list.contains(excluded),
                "`{excluded}`-authored edits do not wake the planner, so the rendered \
                 wake list must not name one; got: {rendered_list}"
            );
        }
        assert!(
            p.contains("你不会被自己（`author = \"planner\"`）的编辑唤醒。"),
            "prompt must still state the self-edit exclusion; got: {p}"
        );
        assert!(
            !p.contains("只有用户的会"),
            "prompt must not claim only user edits wake the planner; got: {p}"
        );
    }

    /// #1211 S3 — the prompt is not the guard and the guard is not the
    /// prompt; both have to exist. `mcp_track_rename` pins the guard. This
    /// pins the instruction, because a `calm.track.rename` no agent is ever
    /// told about would leave every track named `Untitled` with a green test
    /// suite: S1 deleted the only other thing that ever named a track.
    ///
    /// It also pins the name-once *expectation*, not just the tool name. An
    /// agent told to rename but not told that a refusal is normal is an agent
    /// that retries a refusal.
    #[test]
    fn planner_prompt_instructs_the_agent_to_name_the_track() {
        let p = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-naming");
        assert!(
            p.contains("calm.track.rename"),
            "planner prompt must name the naming tool"
        );
        // The instruction is CONDITIONAL on observed state, not a blanket
        // "every track is unnamed": child tracks are born titled from their
        // parent task's goal, and a create request may still carry a title,
        // so an unconditional "rename it" instruction buys a guaranteed
        // `already_named` refusal — a wasted write attempt on every such track.
        assert!(
            p.contains("If `neige state` shows this track's title is still empty"),
            "planner prompt must condition naming on the observed empty title"
        );
        assert!(
            p.contains("If it already carries a title") && p.contains("not call the tool"),
            "planner prompt must tell the agent to skip the call on an already-titled track"
        );
        assert!(
            !p.contains("A track is created unnamed") && !p.contains("nobody has named it yet"),
            "planner prompt must not claim every track starts unnamed"
        );
        assert!(
            p.contains("Naming is name-once"),
            "planner prompt must state the name-once rule"
        );
        assert!(
            p.contains("already_named") && p.contains("that is not an error"),
            "planner prompt must tell the agent a refusal is normal, not a retry signal"
        );
        // The instruction belongs to the per-turn action list, not to some
        // decorative preamble: it has to sit inside step 2, where the agent
        // decides what to do.
        let step2 = p
            .find("2. Decide what to do next and act:")
            .expect("step 2 is present");
        let step3 = p.find("3. **END YOUR TURN.**").expect("step 3 is present");
        let naming = p
            .find("calm.track.rename")
            .expect("naming instruction present");
        assert!(
            step2 < naming && naming < step3,
            "the naming instruction must live inside step 2's action list"
        );
    }

    #[test]
    fn planner_prompt_documents_claude_plan_kind_and_gate_policy() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("(`codex`, `claude`, or `terminal`)"),
            "planner prompt must advertise the accepted task kinds"
        );
        assert!(
            p.contains("Every codex or claude task should declare a verification `gate`"),
            "planner prompt must require gates for both agent/code worker kinds"
        );
        assert!(
            p.contains("terminal tasks are exempt"),
            "planner prompt must not imply terminal tasks require gates"
        );
    }

    /// The reviewed ordinary assistant prompt, byte for byte. The #1343
    /// follow-up corrects its shared mechanics after a real turn proved the
    /// previous prompt advertised planner/worker-only CLI reads and omitted
    /// deferred MCP discovery.
    const ASSISTANT_PROMPT_GOLDEN: &str = include_str!("../tests/goldens/assistant_prompt.txt");

    /// #1343's launchpad identity, byte for byte.
    const LAUNCHPAD_ASSISTANT_PROMPT_GOLDEN: &str =
        include_str!("../tests/goldens/assistant_prompt_launchpad.txt");

    /// Equality against a whole document, not a keyword list: both assistant
    /// identities share the mechanics macro, and a stray newline at either
    /// seam is exactly the kind of change a `contains` check cannot see.
    #[test]
    fn the_ordinary_assistant_prompt_matches_its_reviewed_golden() {
        assert_eq!(
            render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, "track-golden-1189"),
            ASSISTANT_PROMPT_GOLDEN,
        );
    }

    /// #1343 — the launchpad identity, pinned, and pinned as *different*.
    ///
    /// Three assertions, and the last two are what make the first mean
    /// something. The whole-document equality would be satisfied by a golden
    /// regenerated from a launchpad template that had quietly become the
    /// ordinary one; `assert_ne!` against the ordinary prompt is what rules
    /// that out, and it is the assertion the "delete the launchpad branch"
    /// mutation is aimed at from the adapter side.
    ///
    /// The mechanics are asserted shared rather than described as shared: the
    /// marker protocol is the same paragraph in both, so a fork that drifted on
    /// the CAS handshake would be a real defect and this says so.
    #[test]
    fn the_launchpad_assistant_prompt_owns_the_report_and_keeps_the_mechanics() {
        let launchpad = render_system_prompt(
            LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
            "track-golden-1189",
        );
        assert_eq!(launchpad, LAUNCHPAD_ASSISTANT_PROMPT_GOLDEN);

        let ordinary = render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, "track-golden-1189");
        assert_ne!(
            launchpad, ordinary,
            "the launchpad identity has to differ from the ordinary one; if it \
             does not, nothing about #1343 shipped"
        );
        // The sentence that measurably stopped the agent writing: true on an
        // ordinary track, false here.
        assert!(ordinary.contains("you are a guest in a document"));
        assert!(!launchpad.contains("you are a guest in a document"));
        // …and the mechanics really are one paragraph, not two that can drift.
        let markers = "1. Call `calm.report.read` with `with_markers: true` FIRST.";
        assert!(ordinary.contains(markers) && launchpad.contains(markers));
    }

    #[test]
    fn assistant_prompts_match_their_actual_read_and_tool_discovery_surface() {
        let ordinary = render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, "track-golden-1189");
        let launchpad = render_system_prompt(
            LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
            "track-golden-1189",
        );
        for prompt in [ordinary, launchpad] {
            assert!(
                !prompt.contains("`neige state`")
                    && !prompt.contains("`neige ls`")
                    && !prompt.contains("`neige cat`"),
                "Assistant is rejected from planner/worker-only neige reads"
            );
            assert!(
                prompt.contains("use tool search to load that exact `calm.*` tool"),
                "deferred MCP tools must be discovered before declaring them unavailable"
            );
        }
    }

    #[test]
    fn planner_prompt_pins_callable_task_block_protocol() {
        let p = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-contract");
        validate_planner_prompt_contract(&p).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            p.contains("block write still succeeds")
                && p.contains("`gate_required` diagnostic")
                && p.contains("not projected or scheduled")
                && p.contains("unless it provides `no_gate_reason`")
                && p.contains("terminal tasks are exempt"),
            "prompt must describe diagnostic gate admission semantics"
        );
    }

    #[test]
    fn planner_prompt_contract_rejects_negative_context() {
        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-contract");

        let negated = prompt.replace(
            TASK_BLOCK_PROTOCOL_GOLDEN,
            &format!(
                "Never follow this obsolete rule: {TASK_BLOCK_PROTOCOL_GOLDEN} Swap those anchors instead."
            ),
        );
        assert_ne!(
            negated, prompt,
            "negative-context fixture must alter the prompt"
        );
        assert!(
            validate_planner_prompt_contract(&negated).is_err(),
            "correct tokens inside a negated paragraph must not satisfy the contract"
        );
    }

    /// #1185 §1.5 A — direct report edits require an unconditional first read.
    ///
    /// The policy that governs a report now travels inside the report, so an
    /// agent that has not read the document does not know the rules it is
    /// about to break. The old sentence gated the read on
    /// `report_startup_read_required`, which is false for every default track —
    /// exactly the tracks that only learn their contract by reading.
    #[test]
    fn planner_prompt_mandates_first_read_for_direct_edits_and_exempts_dispatch() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;
        assert!(p.contains(
            "The bounded `calm.task.dispatch` creation below does not require this report read."
        ));
        let step1 = p
            .find("1. A kernel recovery decision briefing")
            .expect("step 1 permits deciding from the kernel recovery snapshot");
        assert!(p.contains("When that is sufficient for the recovery decision, act on it without a preliminary state or plan-list read"));
        assert!(p.contains("Run `neige state` for state-dependent decisions"));
        let read = p
            .find("Before you directly edit the report in a session, call `calm.report.read` once")
            .expect("unconditional first-read sentence is present");
        let step2 = p
            .find("2. Decide what to do next and act:")
            .expect("step 2 is present");
        assert!(
            step1 < read && read < step2,
            "the report first-read contract must remain in step 1 despite the recovery briefing exception"
        );
        assert!(
            !p.contains("If `report_startup_read_required` is true, first call"),
            "the read must not be conditional on the startup bit (#1185 §1.5 A)"
        );
        // The bit survives with a narrower meaning: "does it hold content
        // beyond the default skeleton", not "must you read".
        assert!(p.contains("`report_startup_read_required` tells you whether it already holds"));
        // Activation is scoped to `task` blocks; prose is maintained, not
        // replaced — the fork path used to be ordered to flatten it.
        assert!(p.contains(
            "If the read returns `task` blocks, treat them as the authoritative pre-set plan"
        ));
        assert!(p.contains("Prose blocks are NOT a plan to activate"));

        assert!(p.contains("authoritative pre-set plan"));
        assert!(p.contains("replacing those blocks and setting `ready: true`"));
        assert!(p.contains("block ids and revision as replace anchors"));
        assert!(p.contains("Do not mint duplicate tasks"));
    }

    #[test]
    fn planner_prompt_teaches_named_candidate_dispatch_without_report_edit() {
        let p = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-candidate");
        for text in [
            "workspace: \"verified-candidate\"",
            "required input:",
            "input.producer must be the returned repair_key",
            "Reviewing already schedules",
            "current.candidate_input",
            "without mandatory Planner acceptance",
        ] {
            assert!(
                p.contains(text),
                "missing candidate dispatch guidance: {text}"
            );
        }
        assert!(!p.contains("Tasks needing dependencies, gates, file delivery or other options"));
    }

    /// #293 cutover — the planner prompt must be push-native, not pull. It must
    /// carry the turn-reactive guidance (driven by pushed observations, end
    /// the turn, no looping).
    #[test]
    fn planner_prompt_is_push_native_not_pull() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        // No pull loop.
        assert!(
            !p.contains("long-poll"),
            "prompt must not describe a long-poll loop"
        );

        // Turn-reactive guidance present.
        assert!(
            p.contains("turn-reactive") || p.contains("END YOUR TURN"),
            "prompt must carry turn-reactive guidance"
        );
        assert!(
            p.contains("END YOUR TURN"),
            "prompt must tell the agent to end its turn"
        );
        assert!(
            p.contains("re-invoked"),
            "prompt must explain the kernel re-invokes the agent per observation"
        );
        assert!(
            p.contains("Do NOT poll or loop"),
            "prompt must forbid polling / looping"
        );
        // Reads go through the shell CLI; writes still go through MCP.
        assert!(
            p.contains("Run `neige state`")
                && p.contains("calm.report.blocks.upsert")
                && p.contains("calm.plan.list"),
            "prompt must read state via neige and maintain task blocks via MCP"
        );
        assert!(
            !p.contains("calm.update_track_state")
                && p.contains("calm.task.dispatch")
                && !p.contains("calm.plan.upsert")
                && p.contains("calm.plan.cancel")
                && p.contains("calm.plan.list")
                && p.contains("calm.report.blocks.upsert")
                && p.contains("calm.task.verdict")
                && p.contains("calm.area.outline")
                && p.contains("calm.report.links.backlinks")
                // Signature-anchored: bare "calm.report.write" is now also a
                // prefix of "calm.report.write_markdown", so the loose form
                // would pass even if the compatibility tool disappeared.
                && p.contains("calm.report.write(body,")
                && p.contains("calm.report.edit(old_string,")
                && p.contains("calm.report.write_markdown"),
            "prompt must document retained track/task write tools and omit retired update_track_state"
        );
        assert!(
            !p.contains("Call `calm.track.state`"),
            "prompt must not instruct state reads via MCP"
        );
    }

    #[test]
    fn planner_prompt_documents_neige_reads_for_worker_outputs() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        assert!(p.contains(
            "use a sufficient report preview without a preliminary state or result reread"
        ));
        assert!(p.contains("State-dependent actions still require fresh authority"));
        assert!(p.contains("Report arrival, execution settlement, independent verification, and Planner acceptance are distinct"));
        assert!(p.contains("require its recorded event identity"));
        assert!(!p.contains("canonical first read"));
        assert!(!p.contains("push observation is just a notification"));
        assert!(p.contains("the exact `neige cat runs/K/gates/N.log` path in that observation"));
        assert!(!p.contains("plan/<key>/output"));
        assert!(p.contains("opaque execution/attempt ID, not a logical task key"));
        assert!(p.contains("also read `neige cat runs/K.json`"));
        assert!(p.contains("If B needs your semantic decision on A, keep B `ready: false`"));
        assert!(p.contains("`depends_on` waits for `Task.done`, not a Planner verdict"));
        assert!(p.contains("Pure ordering dependencies need no manual verdict"));

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "planner prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("neige cat report.md"),
            "planner prompt must explain why the body-only neige view cannot supply an anchor"
        );
        assert!(
            p.contains("runs/<attempt_id>"),
            "planner prompt must document run projections by execution attempt id"
        );
        assert!(
            p.contains("plan/<key>/gate.log"),
            "planner prompt must document plan gate logs"
        );
        assert!(
            p.contains("READ-ONLY"),
            "planner prompt must state track file views are read-only"
        );
        assert!(
            p.contains("runs/K.md"),
            "planner prompt must document the optional run summary view"
        );
        assert!(
            p.contains("calm.report.write(body,") && p.contains("calm.report.edit(old_string,"),
            "planner prompt must document report write/edit MCP tools"
        );
        assert!(
            p.contains("calm.area.outline")
                && p.contains("calm.report.links.backlinks")
                && !p.contains("calm.track.cat")
                && !p.contains("calm.track.ls")
                && p.contains("calm.report.read"),
            "planner prompt must include the anchored report read alongside retained read tools"
        );
        assert!(
            p.contains("[label](neige://wave/<track_id>#<block_id>)"),
            "planner prompt must pin the cross-reference form"
        );
    }

    #[test]
    fn planner_prompt_pins_whole_document_revision_anchor_contract() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;
        assert!(p.contains("`calm.report.read` 返回的 `docRev`") && p.contains("`if_doc_rev`"));
        assert!(p.contains("写响应会返回新的 `docRev`"));
        assert!(p.contains("块级 `if_rev`") && p.contains("不可混用"));
    }

    /// #1185 — the kernel prompt must name NO report section.
    ///
    /// Section vocabulary is policy: it belongs to the document, which carries
    /// it in a leading HTML comment that every read returns. A prompt that
    /// names sections re-imposes one template's shape on every document in the
    /// area, and the "rewrite anything unfamiliar" instruction that used to
    /// accompany it flattened any report that arrived with its own structure.
    ///
    /// The negative loop at the bottom is this slice's main invariant.
    #[test]
    fn planner_prompt_carries_no_section_vocabulary() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        // The mechanism the prompt keeps: structure travels with the document,
        // and flattening it is damage.
        assert!(
            p.contains("报告自带的结构就是规则"),
            "prompt must state that the document's own structure is the rule"
        );
        assert!(
            p.contains("不要因为格式看起来陌生或「旧」就整体重写本文档"),
            "prompt must forbid flattening an unfamiliar-looking report"
        );

        // The section ban must be QUALIFIED by the document's own contract
        // list. Unqualified it contradicts every shipped template:
        // their seeded body carries a single `# Plan` H1, and the contract
        // inside it requires the agent to add 概要 / 已完成 / 决策. An absolute
        // "never add a section" bullet and the "文档里的维护契约优先" fallback
        // two lines below cannot both be obeyed — this keeps them aligned with
        // `track_report_section_rules.md`'s own wording.
        assert!(
            p.contains("不要新增文档契约清单以外的章节"),
            "the section ban must be scoped to the document's contract list (#1185 D2)"
        );
        assert!(
            !p.contains("不要新增、重命名章节"),
            "an unqualified section ban contradicts the shipped templates' own contracts"
        );

        // `# 进行中` was dropped in #1172: the TASKS panel renders the real
        // task runtime state, so making the planner agent hand-maintain a prose
        // mirror of it every turn is pure LLM restatement of kernel-known,
        // already-rendered data. It must not come back via the skeleton either.
        assert!(
            !p.contains("# 进行中"),
            "prompt must NOT reintroduce `# 进行中` — task runtime state is owned by the TASKS panel"
        );
        assert!(
            !crate::track_report::TrackReportPayload::initial()
                .body
                .contains("# 进行中"),
            "the birth skeleton must NOT reintroduce `# 进行中` either"
        );

        // Append-to-progress was the wording that drove the runaway journal.
        assert!(
            !p.contains("append to `# Progress`"),
            "prompt must NOT instruct append-to-progress (root cause of runaway journals)"
        );

        // #1146 S1: the budget must scope to PROSE, not `body`. `body` is the
        // flat projection that also serializes every non-prose block's fence,
        // so a `body`-scoped budget was vacuously false on any track with task
        // blocks — no amount of concise prose could satisfy it.
        //
        // #1185 splits it: the 1000-word soft target is genre judgement and
        // moved into the document's contract. #1571: the contract's own budget
        // governs (the research contract says 1500—2500 字); the kernel's
        // 2000 字 is the fallback only when a contract states no budget, not a
        // ceiling laid over every contract.
        assert!(
            p.contains("散文正文")
                && p.contains("字数预算以文档自己的维护契约为准")
                && p.contains("契约没有规定篇幅时")
                && p.contains("2000 字"),
            "prompt must defer the prose budget to the document's contract, 2000 字 as fallback"
        );
        assert!(
            !p.contains("硬上限") && !p.contains("无论文档自己的契约怎么说"),
            "prompt must not override the contract's budget with a kernel ceiling"
        );
        assert!(
            p.contains("不计入"),
            "prompt must state that non-prose fence projection is excluded from the budget"
        );
        assert!(
            !p.contains("body 控制在"),
            "prompt must NOT reintroduce the vacuous body-scoped budget"
        );

        // The migration instruction is gone, not relocated: it is what
        // flattened self-structured reports.
        assert!(
            !p.contains("整体 REWRITE"),
            "prompt must NOT order a wholesale rewrite of an existing report (#1185)"
        );

        // —— the main invariant ——
        for banned in [
            "# 概要",
            "# 待你定",
            "# 已完成",
            "# 决策",
            "# Goal",
            "# Progress",
            "# Needs attention",
            "# Results",
            "# Timeline",
        ] {
            assert!(
                !p.contains(banned),
                "planner prompt must not name a report section — structure travels with the document (#1185): {banned}"
            );
        }
    }

    /// #1146 S1 — whole-document rewrites must go through the ONLY
    /// id-preserving mouth: `calm.report.read { with_markers: true }` →
    /// `calm.report.write_markdown`. `calm.report.write` re-derives block ids
    /// best-effort (`reassign_ids`) and its new body must carry every
    /// non-prose fence back byte-for-byte or `guard_non_prose_stomp` rejects
    /// the write, so it must NOT be advertised as the preferred mouth.
    #[test]
    fn planner_prompt_routes_whole_document_rewrite_through_the_marker_channel() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("calm.report.write_markdown"),
            "prompt must name the id-preserving whole-document write tool"
        );
        assert!(
            p.contains("with_markers"),
            "prompt must name the `with_markers` read that supplies the block-id markers"
        );
        assert!(
            p.contains("<!-- neige:b_xxxx -->"),
            "prompt must show the marker line shape the read emits"
        );
        // Targeted edits stay the first choice.
        assert!(
            p.contains("**首选 · 局部修改** — `calm.report.blocks.upsert`"),
            "prompt must make block-addressed upsert the preferred write"
        );
        // The trap must be spelled out, not merely de-emphasized.
        assert!(
            p.contains("best-effort 重新推导块 id"),
            "prompt must warn that wholesale replace re-derives block ids"
        );
        assert!(
            p.contains("neige-block <kind>") && p.contains("逐字节原样"),
            "prompt must warn that non-prose fences must survive byte-for-byte"
        );
        // The old wording promoted `calm.report.write` as 首选 — that is the
        // exact trap this slice removes.
        assert!(
            !p.contains("整体替换 （首选"),
            "prompt must NOT re-promote calm.report.write as the preferred write"
        );
        // Planner feedback #1 — one user-intent update is ONE
        // `calm.report.commit` (blocks + summary + lifecycle under one
        // `if_doc_rev`); the prompt must route message/lifecycle there and
        // must no longer sanction same-text `calm.report.edit` as a
        // lifecycle carrier.
        assert!(
            p.contains("**一次用户意图 = 一次 `calm.report.commit`**")
                && p.contains("calm.report.commit(if_doc_rev,")
                && p.contains("任一项失败整次提交回滚"),
            "prompt must route blocks + summary + lifecycle through calm.report.commit"
        );
        assert!(
            p.contains("不要用 `calm.report.edit` 传相同的 old/new 字符串"),
            "prompt must forbid same-text report.edit as a lifecycle carrier"
        );
        assert!(
            !p.contains("不接受这两个参数") && !p.contains("或需要带上"),
            "prompt must not keep the pre-commit message/lifecycle routing"
        );
    }

    #[test]
    fn worker_prompt_documents_neige_read_cli() {
        let p = WORKER_SYSTEM_PROMPT_PLACEHOLDER;

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "worker prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("neige task-completed") && p.contains("neige task-failed"),
            "worker prompt must document task completion through the neige CLI"
        );
        assert!(
            p.contains("completion report is a claim")
                && p.contains("kernel gate may verify it")
                && p.contains("idempotency key the kernel handed you"),
            "worker prompt must describe gate verification and kernel-provided idempotency key"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-track-only"),
            "worker prompt must constrain neige reads to read-only own-track views"
        );
    }

    /// #838 Move 2 — the codex worker prompt reports completion through the
    /// native MCP tools, NOT the `neige task-completed`/`task-failed` CLI.
    /// claude keeps the CLI (covered by the const tests above + the
    /// claude_adapter contract test), so this is the codex-only divergence.
    #[test]
    fn worker_codex_prompt_reports_completion_via_mcp_tools_not_cli() {
        let p = WORKER_CODEX_SYSTEM_PROMPT;

        // Completion is mandated through the native MCP tools.
        assert!(
            p.contains("calm.task.complete") && p.contains("calm.task.fail"),
            "codex worker prompt must mandate the calm.task.complete / calm.task.fail MCP tools"
        );
        // It must NOT mandate the neige completion CLI (that is claude-only).
        assert!(
            !p.contains("neige task-completed") && !p.contains("neige task-failed"),
            "codex worker prompt must NOT mandate the neige completion CLI"
        );
        // Reads still ride the neige CLI for BOTH providers (shared tail).
        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "codex worker prompt must keep the neige read CLI in the shared tail"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-track-only"),
            "codex worker prompt must keep the read-only own-track constraint"
        );
        // The required-arg wording matches the tool schemas: complete needs
        // `idempotency_key`; fail needs `idempotency_key` + a required `reason`.
        assert!(
            p.contains("idempotency_key") && p.contains("required"),
            "codex worker prompt must name idempotency_key and the required reason"
        );
    }

    /// The provider split must not change the claude (CLI) body: the codex
    /// and claude worker prompts share everything except step 3, so the
    /// shared `## Reading track state` tail must be byte-identical in both.
    #[test]
    fn worker_prompts_share_identical_reads_tail() {
        let marker = "## Reading track state";
        let cli_tail = WORKER_SYSTEM_PROMPT_PLACEHOLDER
            .split_once(marker)
            .map(|(_, tail)| tail)
            .expect("CLI worker prompt has a reads tail");
        let mcp_tail = WORKER_CODEX_SYSTEM_PROMPT
            .split_once(marker)
            .map(|(_, tail)| tail)
            .expect("codex worker prompt has a reads tail");
        assert_eq!(
            cli_tail, mcp_tail,
            "both worker prompts must share a byte-identical reads tail"
        );
    }
}
