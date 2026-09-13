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
//!   2. The worker and assistant prompts, likewise data under
//!      `prompts/worker/` and `prompts/assistant/` (#1635 S1b), assembled
//!      with `concat!` + `include_str!` so shared parts exist once.
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

/// Worker-agent system prompt for the **claude** (CLI-completion) provider.
/// PR8 (#136) replaced the PR6 stub with the production prompt: workers are
/// short-lived, fire-and-forget, driven by the kernel scheduler from the
/// planner-maintained plan. They run one job and exit.
///
/// The prose is data (#1635 S1b): `prompts/worker/head-cli.md` is everything
/// before the shared reads tail and `prompts/worker/tail.md` is that tail,
/// shared byte-for-byte with [`WORKER_CODEX_SYSTEM_PROMPT`]. Both are embedded
/// at compile time; `concat!` keeps the const `&'static str` with no runtime
/// allocation and no second copy of the tail that could go stale.
///
/// The name retains the `_PLACEHOLDER` suffix only to avoid churn in
/// downstream call sites; the content is production. A followup can rename
/// this to `WORKER_SYSTEM_PROMPT_TEMPLATE` for symmetry with
/// [`PLANNER_SYSTEM_PROMPT_TEMPLATE`] when there's no other PR touching this
/// file.
///
/// Wording is pinned by the whole-document golden
/// `tests/goldens/worker_prompt_cli.txt` (regenerate with
/// `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff).
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str = concat!(
    include_str!("../prompts/worker/head-cli.md"),
    include_str!("../prompts/worker/tail.md")
);

/// codex worker variant (#838 Move 2): `prompts/worker/head-mcp.md` plus the
/// same `prompts/worker/tail.md`. It differs from
/// [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] only in how completion is reported:
/// through the native `calm.task.complete` / `calm.task.fail` MCP tools
/// (channel 2 — DaemonTrust + codex-injected `_meta.threadId`) instead of the
/// `neige` shell CLI. This decouples the kernel-critical completion path from
/// the per-thread `shell_environment_policy` env (channel 3) that keeps
/// getting silently dropped (#738/#747/#836).
///
/// claude keeps [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] (it has no codex thread
/// to authenticate against — the native-MCP resolver is
/// `AgentProvider::Codex`-only — and `claude_adapter`'s contract test asserts
/// the CLI surface). Reads stay on the `neige` shell CLI for both providers
/// (#339/#377 read-via-CLI principle), which is why the tail is one file
/// concatenated into both consts.
///
/// Wording is pinned by `tests/goldens/worker_prompt_mcp.txt`.
pub(crate) const WORKER_CODEX_SYSTEM_PROMPT: &str = concat!(
    include_str!("../prompts/worker/head-mcp.md"),
    include_str!("../prompts/worker/tail.md")
);

/// #1189 — the track assistant's system prompt: `prompts/assistant/
/// ordinary-head.md` (identity), `prompts/assistant/mechanics.md` (the tool
/// surface and marker protocol shared by **both** assistant identities), and
/// `prompts/assistant/ordinary-tail.md` (the closing paragraph that is true
/// only on an ordinary track — see [`LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE`]
/// for why it is its own file). Embedded at compile time so the const stays
/// `&'static str`.
///
/// Deliberately not a trimmed copy of [`PLANNER_SYSTEM_PROMPT_TEMPLATE`]: most of
/// that prompt instructs the agent to drive the lifecycle state machine and the
/// plan, and every one of those tools rejects `CardRole::Assistant` at the
/// handler. Describing them here would teach the agent to spend turns on calls
/// that can only come back `-32602`.
///
/// Two things in the mechanics are load-bearing rather than stylistic:
///
/// * **read with markers before you rewrite** — a `calm.report.write` style
///   full-document rewrite is unavailable to this role, and a block write that
///   re-mints ids reads as "delete every task block and create new ones", which
///   the task-block guard rejects as a whole transaction (design §3.2a-bis.4).
///   The marker read is what keeps existing block ids stable.
/// * **the assistant does not own the plan** — the guard exists, but an agent
///   that keeps trying to write task blocks produces a stream of rejected turns
///   instead of answering the user.
///
/// #1343 forks the assistant's *identity* — first duty, and who owns the
/// document — and nothing else; keeping the mechanics in one file is what stops
/// the halves that are not in dispute from drifting.
///
/// Wording is pinned by `tests/goldens/assistant_prompt.txt`.
pub(crate) const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/ordinary-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/ordinary-tail.md")
);

/// #1343 — the assistant on **Today's launchpad track**:
/// `prompts/assistant/launchpad-head.md`, the shared
/// `prompts/assistant/mechanics.md`, and `prompts/assistant/launchpad-tail.md`.
///
/// Same tools, same marker protocol, different job. Measured on the 4140
/// preview: told explicitly to write a block, the agent wrote one (`docRev`
/// 1→2), so the tool surface, the CAS handshake and the write permission were
/// all already working. Told casually what had happened, it made zero tool
/// calls and answered in chat. The prompt was the cause, in two places:
///
/// * the ordinary identity's first duty is answering the user, with writing
///   the report listed as a capability, not a duty, so chatting was the
///   default path;
/// * the ordinary closing paragraph describes the agent as a guest in a
///   document the planner agent maintains. On an ordinary track that is true.
///   On the launchpad there is no planner agent writing today's report — by
///   design this conversation is the writer — so the prompt was telling it
///   the document was not its to touch.
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
/// Wording is pinned by `tests/goldens/assistant_prompt_launchpad.txt`.
///
/// [`routes::today::is_launchpad_track`]: crate::routes::today::is_launchpad_track
pub(crate) const LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/launchpad-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/launchpad-tail.md")
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
        // …and the mechanics really are one file, not two that can drift.
        let mechanics = include_str!("../prompts/assistant/mechanics.md");
        assert!(!mechanics.is_empty(), "the shared mechanics file is empty");
        assert!(
            ordinary.contains(mechanics) && launchpad.contains(mechanics),
            "both assistant identities must embed prompts/assistant/mechanics.md"
        );
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
        let mut mismatched = Vec::new();
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
            if rendered != expected {
                let at = rendered
                    .bytes()
                    .zip(expected.bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| rendered.len().min(expected.len()));
                mismatched.push(format!("{file} (first difference at byte {at})"));
            }
        }
        assert!(
            !regen,
            "worker_prompt_cli.txt / worker_prompt_mcp.txt regenerated from the current \
             prompts; hand-verify the diff, commit, and re-run without REGEN_PROMPT_GOLDENS"
        );
        assert!(
            mismatched.is_empty(),
            "worker prompt goldens differ from the rendered prompts: {mismatched:?}; \
             regenerate with REGEN_PROMPT_GOLDENS=1 and hand-verify the diff"
        );
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

    /// Every `calm.*` token in `prompt` (see [`calm_tool_tokens`]) checked
    /// against the tool registry, with every exception explicit and
    /// self-checking:
    ///
    /// * each token is a **registered, non-alias** tool name, whatever role
    ///   it belongs to — a typo, a retired name, or a deprecated alias is red
    ///   no matter what the lists say;
    /// * each token in neither list is **visible to `role`** in `tools/list`
    ///   (`descriptors_for_role`);
    /// * each `callable_but_hidden` entry is registered and NOT visible to
    ///   `role`: the tool's handler admits the role while its descriptor
    ///   does not, so the prompt is the role's only contract for it. An
    ///   entry that became visible is stale and goes red;
    /// * each `named_to_forbid` entry is registered and NOT visible to
    ///   `role`: the prompt names it only to say the role may not call it.
    ///   Same staleness check;
    /// * every entry of either list must actually be named by the prompt —
    ///   an exception nobody uses is dead weight and goes red — and no name
    ///   may sit in both lists;
    /// * anti-vacuity: the prompt names at least `min_named` distinct tools
    ///   (three unless the prompt demonstrably names fewer), on top of every
    ///   listed exception having to be found.
    fn assert_prompt_tool_names(
        label: &str,
        prompt: &str,
        role: calm_types::model::CardRole,
        callable_but_hidden: &[&str],
        named_to_forbid: &[&str],
        min_named: usize,
    ) {
        use std::collections::BTreeSet;

        let registry = crate::mcp_server::build_default_registry();
        let aliases = registry.deprecated_alias_names();
        let registered: BTreeSet<String> = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .filter(|name| !aliases.contains(name))
            .collect();
        let visible: BTreeSet<String> = registry
            .descriptors_for_role(role)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        assert!(
            !visible.is_empty(),
            "the {role:?} role sees no tools at all"
        );
        assert!(
            visible.iter().all(|name| registered.contains(name)),
            "a visible tool is a deprecated alias; aliases must stay hidden"
        );

        let named: BTreeSet<&str> = calm_tool_tokens(prompt).into_iter().collect();
        assert!(
            named.len() >= min_named,
            "anti-vacuity: {label} names fewer than {min_named} distinct tools; the \
             scanner is probably broken. Found: {named:?}"
        );

        for (list, entries) in [
            ("callable_but_hidden", callable_but_hidden),
            ("named_to_forbid", named_to_forbid),
        ] {
            for name in entries {
                assert!(
                    registered.contains(*name),
                    "{label}: `{name}` is listed as {list} but is not a registered tool \
                     (typo, retired, or alias); drop it from the list"
                );
                assert!(
                    !visible.contains(*name),
                    "{label}: `{name}` is listed as {list} but IS visible to {role:?} in \
                     tools/list; the exception is stale, drop it from the list"
                );
                assert!(
                    named.contains(name),
                    "{label}: `{name}` is listed as {list} but the prompt never names it; \
                     drop it from the list"
                );
            }
        }
        for name in callable_but_hidden {
            assert!(
                !named_to_forbid.contains(name),
                "{label}: `{name}` is in both callable_but_hidden and named_to_forbid"
            );
        }

        for name in &named {
            assert!(
                registered.contains(*name),
                "{label} names `{name}`, which is not a registered tool (typo, retired, \
                 alias, or not a complete tool name). Registered: {registered:?}"
            );
            if callable_but_hidden.contains(name) || named_to_forbid.contains(name) {
                continue;
            }
            assert!(
                visible.contains(*name),
                "{label} names `{name}`, which the {role:?} role cannot see in tools/list \
                 (other-role or hidden). If the prompt names it to forbid it, list it \
                 under named_to_forbid; if the role can call it despite the descriptor, \
                 list it under callable_but_hidden. Visible: {visible:?}"
            );
        }
    }

    /// #1635 S1b — the worker prompts, both providers, name only tools the
    /// Worker role can see, except the two Planner-only tools each prompt
    /// names in order to forbid them (`calm.task.dispatch`,
    /// `calm.task.verdict`). The same statement S1a makes for the planner;
    /// the Worker's visible set is pinned exactly by
    /// `tools_list_for_worker_role_returns_completion_tools`.
    #[test]
    fn worker_prompts_name_only_tools_the_worker_role_can_see() {
        // The CLI prompt reports completion through `neige task-completed`,
        // not a `calm.*` tool, so the two forbidden names are the only
        // tokens it has; the codex prompt adds `calm.task.complete` /
        // `calm.task.fail`.
        for (label, template, min_named) in [
            ("CLI worker prompt", WORKER_SYSTEM_PROMPT_PLACEHOLDER, 2),
            ("codex worker prompt", WORKER_CODEX_SYSTEM_PROMPT, 3),
        ] {
            assert_prompt_tool_names(
                label,
                &render_system_prompt(template, "track-registry"),
                calm_types::model::CardRole::Worker,
                &[],
                &["calm.task.dispatch", "calm.task.verdict"],
                min_named,
            );
        }
    }

    /// #1635 S1b — both assistant identities name only tools the Assistant
    /// role can see, with two explicit exceptions:
    ///
    /// * `calm.report.read` is callable but hidden (#1189 F6): its handler
    ///   admits the Assistant — `mcp_assistant_tool_gate::
    ///   assistant_token_can_read_the_report_with_concurrency_tokens` proves
    ///   the call succeeds — while its descriptor is visible to Planner only,
    ///   so `tools_list_for_assistant_role_returns_block_channel_only` pins
    ///   it absent from the Assistant's `tools/list`. The prompt is therefore
    ///   the Assistant's only contract for the read, which is exactly why it
    ///   must keep naming it.
    /// * `calm.report.write` is named to forbid it.
    ///
    /// This replaces the deleted wording test that said the assistant prompt
    /// must not mention planner/worker-only reads: stated against the
    /// registry it covers every name, not the three it listed.
    #[test]
    fn assistant_prompts_name_only_tools_the_assistant_role_can_see() {
        for (label, template) in [
            (
                "ordinary assistant prompt",
                ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
            ),
            (
                "launchpad assistant prompt",
                LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
            ),
        ] {
            assert_prompt_tool_names(
                label,
                &render_system_prompt(template, "track-registry"),
                calm_types::model::CardRole::Assistant,
                &["calm.report.read"],
                &["calm.report.write"],
                3,
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

    /// The provider split is one shared tail plus two distinct heads: both
    /// worker consts end with `prompts/worker/tail.md` byte-for-byte (reads
    /// stay on the `neige` CLI for both providers), and what precedes it
    /// differs (completion is reported differently). Stated against the
    /// file, not a marker string, so a second copy of the tail that drifted
    /// would fail here rather than pass a `contains` check.
    #[test]
    fn worker_prompts_share_identical_reads_tail() {
        let tail = include_str!("../prompts/worker/tail.md");
        assert!(!tail.is_empty(), "the shared reads tail is empty");
        let cli_head = WORKER_SYSTEM_PROMPT_PLACEHOLDER
            .strip_suffix(tail)
            .expect("CLI worker prompt ends with the shared reads tail");
        let mcp_head = WORKER_CODEX_SYSTEM_PROMPT
            .strip_suffix(tail)
            .expect("codex worker prompt ends with the shared reads tail");
        assert!(!cli_head.is_empty() && !mcp_head.is_empty());
        assert_ne!(
            cli_head, mcp_head,
            "the two worker heads must differ (completion channel); if they do \
             not, one provider's prompt was silently wired to the other's head"
        );
    }
}
