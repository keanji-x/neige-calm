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
        for template in [
            PLANNER_SYSTEM_PROMPT_TEMPLATE,
            WORKER_SYSTEM_PROMPT_PLACEHOLDER,
            WORKER_CODEX_SYSTEM_PROMPT,
        ] {
            let out = render_system_prompt(template, "track-abc");
            assert!(
                out.contains("track-abc"),
                "track id should be substituted; got: {out}"
            );
            assert!(
                !out.contains("{track_id}"),
                "placeholder should be gone; got: {out}"
            );
        }
    }

    /// The role → template relation: each seeded role hands out its own
    /// const, and the three consts are distinct documents.
    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        assert_eq!(
            SeededCardRole::Planner.prompt_template(),
            PLANNER_SYSTEM_PROMPT_TEMPLATE
        );
        assert_eq!(
            SeededCardRole::Worker.prompt_template(),
            WORKER_SYSTEM_PROMPT_PLACEHOLDER
        );
        assert_eq!(
            SeededCardRole::WorkerCodex.prompt_template(),
            WORKER_CODEX_SYSTEM_PROMPT
        );
        assert_ne!(
            PLANNER_SYSTEM_PROMPT_TEMPLATE,
            WORKER_SYSTEM_PROMPT_PLACEHOLDER
        );
        assert_ne!(PLANNER_SYSTEM_PROMPT_TEMPLATE, WORKER_CODEX_SYSTEM_PROMPT);
        assert_ne!(WORKER_SYSTEM_PROMPT_PLACEHOLDER, WORKER_CODEX_SYSTEM_PROMPT);
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

    /// #1635 S1b — the two worker prompts, byte for byte, rendered for one
    /// fixed track id. They had no golden before this slice; the move of
    /// their prose out of Rust is proved by these files not changing.
    const WORKER_PROMPT_CLI_GOLDEN: &str = include_str!("../tests/goldens/worker_prompt_cli.txt");
    const WORKER_PROMPT_MCP_GOLDEN: &str = include_str!("../tests/goldens/worker_prompt_mcp.txt");

    /// Whole-document equality for both worker prompts. Regenerate with
    /// `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff: the goldens are
    /// the reviewed wording, so a regen is a review, not a fix.
    #[test]
    fn the_worker_prompts_match_their_reviewed_goldens() {
        let regen = std::env::var_os("REGEN_PROMPT_GOLDENS").is_some();
        for (file, template, golden) in [
            (
                "worker_prompt_cli.txt",
                WORKER_SYSTEM_PROMPT_PLACEHOLDER,
                WORKER_PROMPT_CLI_GOLDEN,
            ),
            (
                "worker_prompt_mcp.txt",
                WORKER_CODEX_SYSTEM_PROMPT,
                WORKER_PROMPT_MCP_GOLDEN,
            ),
        ] {
            let rendered = render_system_prompt(template, "track-golden-1635");
            if regen {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/goldens")
                    .join(file);
                // Write back `rendered + "\n"`: the assertion side does
                // `strip_suffix('\n')`, so omitting it panics on the next run.
                std::fs::write(&path, format!("{rendered}\n")).expect("write regenerated golden");
                continue;
            }
            let expected = golden
                .strip_suffix('\n')
                .expect("text fixture has its repository newline");
            assert_eq!(
                rendered, expected,
                "{file} differs from the rendered prompt"
            );
        }
        assert!(
            !regen,
            "worker_prompt_cli.txt / worker_prompt_mcp.txt regenerated from the current \
             prompts; hand-verify the diff, commit, and re-run without REGEN_PROMPT_GOLDENS"
        );
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

    /// #1185 — the kernel prompt must name NO report section.
    ///
    /// Section vocabulary is policy: it belongs to the document, which carries
    /// it in a leading HTML comment that every read returns. A prompt that
    /// names sections re-imposes one template's shape on every document in the
    /// area. The banned-section loop is the invariant: section names that
    /// once lived in the kernel prompt or skeleton and must not return. The
    /// golden would show such a return as a diff; this test says it is a
    /// policy violation, which a diff cannot.
    #[test]
    fn planner_prompt_carries_no_section_vocabulary() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

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

    /// Every `calm.`-prefixed token in `text`, wherever it appears (prose,
    /// code span, signature): an occurrence of `calm.` whose preceding byte is
    /// not `[A-Za-z0-9_.]`, extended over `[A-Za-z0-9_.]`, with trailing `.`s
    /// stripped. A token whose unstripped end is followed by `*` is a wildcard
    /// family (`calm.*`, `calm.report.blocks.*`) and is dropped. Uppercase is
    /// part of the continuation on purpose: tool names are lowercase, so
    /// `calm.plan.listX` must stay one (unregistered) token rather than
    /// truncate to a registered prefix. Hand-rolled on purpose: no regex
    /// dependency for one test.
    fn calm_tool_tokens(text: &str) -> Vec<&str> {
        let bytes = text.as_bytes();
        let mut tokens = Vec::new();
        let mut from = 0;
        while let Some(i) = text[from..].find("calm.") {
            let at = from + i;
            let preceded_by_ident = at > 0 && {
                let b = bytes[at - 1];
                b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
            };
            let end = at
                + bytes[at..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'.')
                    .count();
            from = end;
            if preceded_by_ident || bytes.get(end) == Some(&b'*') {
                continue;
            }
            tokens.push(text[at..end].trim_end_matches('.'));
        }
        tokens
    }

    #[test]
    fn calm_tool_tokens_are_whole_tokens() {
        for (text, expected) in [
            ("see calm.plan.list.", vec!["calm.plan.list"]),
            ("xcalm.plan.list", vec![]),
            ("calm.*", vec![]),
            ("calm.report.blocks.*", vec![]),
            ("calm.plan.list2", vec!["calm.plan.list2"]),
            ("calm.plan.listX", vec!["calm.plan.listX"]),
            ("", vec![]),
            ("calm.", vec!["calm"]),
        ] {
            assert_eq!(calm_tool_tokens(text), expected, "input: {text:?}");
        }
    }

    /// #1635 S1a — every `calm.`-prefixed token anywhere in the rendered
    /// planner prompt, backticked or bare, is the complete name of a tool the
    /// Planner role can see in `tools/list`. The deleted per-name asserts
    /// stated this one tool at a time (no retired `calm.update_track_state`,
    /// no hidden `calm.plan.upsert`, no CLI-only `calm.track.cat` /
    /// `calm.track.ls`); stated once against the registry it also covers the
    /// names nobody thought to ban. Tokens are whole-token matched, so a
    /// misspelling or a stray suffix (`calm.plan.list2`) is red, not a prefix
    /// hit; only wildcard families (`calm.*`, `calm.report.blocks.*`) are
    /// skipped.
    #[test]
    fn planner_prompt_names_only_tools_the_planner_role_can_see() {
        use std::collections::BTreeSet;

        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-registry");
        let visible: BTreeSet<String> = crate::mcp_server::build_default_registry()
            .descriptors_for_role(calm_types::model::CardRole::Planner)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        assert!(!visible.is_empty(), "the Planner role sees no tools at all");

        let named: BTreeSet<&str> = calm_tool_tokens(&prompt).into_iter().collect();
        assert!(
            named.len() >= 10,
            "anti-vacuity: the planner prompt names fewer than 10 distinct tools; \
             the scanner is probably broken. Found: {named:?}"
        );
        for name in &named {
            assert!(
                visible.contains(*name),
                "planner prompt names `{name}`, which the Planner role cannot see in \
                 tools/list (retired, hidden, CLI-only, or not a complete tool name). \
                 Visible: {visible:?}"
            );
        }
    }

    /// #1635 S1a — the task `kind` vocabulary the prompt teaches is
    /// `WorkerProviderKind`, spelled as its wire/DB string. The match is
    /// exhaustive on purpose: a new variant fails to compile at the match,
    /// which points a maintainer at the list next to it; every listed kind
    /// then has to be named by the prompt.
    #[test]
    fn planner_prompt_names_every_worker_provider_kind() {
        use calm_types::worker::WorkerProviderKind;

        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-kinds");
        for kind in [
            WorkerProviderKind::Codex,
            WorkerProviderKind::Claude,
            WorkerProviderKind::Terminal,
        ] {
            match kind {
                WorkerProviderKind::Codex
                | WorkerProviderKind::Claude
                | WorkerProviderKind::Terminal => {}
            }
            let spelled = format!("`{}`", kind.as_db_str());
            assert!(
                prompt.contains(&spelled),
                "planner prompt must name task kind {spelled}"
            );
        }
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
