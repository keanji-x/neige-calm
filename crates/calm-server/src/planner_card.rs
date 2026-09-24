//! Planner-card binding: the role-specific system prompts (data under `prompts/`, embedded at compile time) and their per-spawn placeholder substitution.

/// The planner-agent system prompt template, embedded from `prompts/planner.md`. Placeholders `{track_id}` and `{planner_wake_authors}` are substituted by [`render_system_prompt`].
/// Wording is pinned by `tests/goldens/issue_development_planner_prompt.txt` (regenerate with `REGEN_PLANNER_PROMPT_GOLDEN=1`, then hand-verify the diff).
pub(crate) const PLANNER_SYSTEM_PROMPT_TEMPLATE: &str = include_str!("../prompts/planner.md");

/// Worker-agent system prompt for the **claude** (CLI-completion) provider; `prompts/worker/tail.md` is shared byte-for-byte with [`WORKER_CODEX_SYSTEM_PROMPT`].
/// Wording is pinned by `tests/goldens/worker_prompt_cli.txt` (regenerate with `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff).
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str = concat!(
    include_str!("../prompts/worker/head-cli.md"),
    include_str!("../prompts/worker/tail.md")
);

/// codex worker variant: differs from [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] only in reporting completion through the native `calm.task.complete` / `calm.task.fail` MCP tools instead of the `neige` shell CLI. Pinned by `tests/goldens/worker_prompt_mcp.txt`.
pub(crate) const WORKER_CODEX_SYSTEM_PROMPT: &str = concat!(
    include_str!("../prompts/worker/head-mcp.md"),
    include_str!("../prompts/worker/tail.md")
);

/// The track assistant's system prompt: ordinary head, the mechanics shared by both assistant identities, and the ordinary tail.
/// Deliberately not a trimmed planner prompt: every lifecycle/plan tool rejects `CardRole::Assistant` at the handler. Pinned by `tests/goldens/assistant_prompt.txt`.
pub(crate) const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/ordinary-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/ordinary-tail.md")
);

/// The assistant on Today's launchpad track: same mechanics, inverted identity (this conversation is the report's writer). Selected by `routes::today::is_launchpad_track` at `thread/start`.
/// `developer_instructions` are handed over at thread start, so an existing conversation keeps the identity it was started with. Pinned by `tests/goldens/assistant_prompt_launchpad.txt`.
pub(crate) const LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/launchpad-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/launchpad-tail.md")
);

/// Render the report-edit authors that wake the planner, in the wire spelling the `track.report_edited` payload carries.
fn planner_wake_authors_prose() -> String {
    crate::dispatcher::PLANNER_WAKE_AUTHORS
        .iter()
        .map(|author| format!("`{}`", author.wire_str()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Substitute the per-spawn placeholders `{track_id}` and `{planner_wake_authors}` into a prompt template.
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
    "cancel a pending task, or a running codex/claude task whose worker the kernel ",
    "then stops; dispatched, verifying and terminal-kind tasks cannot be canceled. ",
    "Use `calm.plan.list` to inspect status. ",
    "A `codex`/`claude` task requires `goal`, a natural-language objective, and ",
    "forbids `command`. A `terminal` task requires `command`, the exact Shell ",
    "command passed verbatim to `/bin/sh -c`, and forbids `goal`."
);

/// Exact paragraph oracle for the static task-block protocol; free-text contradictions cannot be proved absent with a keyword list.
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

/// Test-only seam: the rendered worker prompt for the provider under test. Doc-hidden so it does not widen the public prompt API.
#[doc(hidden)]
pub fn render_worker_prompt_for_e2e(track_id: &str, codex: bool) -> String {
    let role = if codex {
        SeededCardRole::WorkerCodex
    } else {
        SeededCardRole::Worker
    };
    render_system_prompt(role.prompt_template(), track_id)
}

/// Test-only seam: the exact `developer_instructions` string a track assistant's `thread/start` must carry, so the test asserts equality against production's own value.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_assistant_prompt_for_test(track_id: &str) -> String {
    render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, track_id)
}

/// The same seam for the launchpad assistant's identity; its own function so a test asserts on which template shipped, not on a flag.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_launchpad_assistant_prompt_for_test(track_id: &str) -> String {
    render_system_prompt(LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE, track_id)
}

/// Roles that legitimately need role-specific Codex setup; carved out of `CardRole` so the seeding helper cannot be handed a role with no template.
/// User-facing Worker cards use `routes::codex_cards`'s simpler seed path and must not reach this helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeededCardRole {
    Planner,
    /// Worker card for a **claude** provider: completion is reported through the `neige` shell CLI.
    Worker,
    /// Worker card for a **codex** provider: completion is reported through the native `calm.task.complete` / `calm.task.fail` MCP tools.
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

    /// The expected wire spellings are pinned here on purpose: they are the independent statement that catches a silent shrink of the const.
    #[test]
    fn planner_prompt_renders_the_dispatcher_report_edit_wake_set() {
        let p = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-wake");

        assert!(
            !p.contains("{planner_wake_authors}"),
            "wake-author placeholder must be substituted; got: {p}"
        );
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

    const ASSISTANT_PROMPT_GOLDEN: &str = include_str!("../tests/goldens/assistant_prompt.txt");

    const LAUNCHPAD_ASSISTANT_PROMPT_GOLDEN: &str =
        include_str!("../tests/goldens/assistant_prompt_launchpad.txt");

    /// Whole-document equality: a stray newline at either seam is what a `contains` check cannot see.
    #[test]
    fn the_ordinary_assistant_prompt_matches_its_reviewed_golden() {
        assert_eq!(
            render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, "track-golden-1189"),
            ASSISTANT_PROMPT_GOLDEN,
        );
    }

    /// `assert_ne!` against the ordinary prompt rules out a golden regenerated from a launchpad template that had quietly become the ordinary one.
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
        let mechanics = include_str!("../prompts/assistant/mechanics.md");
        assert!(!mechanics.is_empty(), "the shared mechanics file is empty");
        assert!(
            ordinary.contains(mechanics) && launchpad.contains(mechanics),
            "both assistant identities must embed prompts/assistant/mechanics.md"
        );
    }

    const WORKER_PROMPT_CLI_GOLDEN: &str = include_str!("../tests/goldens/worker_prompt_cli.txt");
    const WORKER_PROMPT_MCP_GOLDEN: &str = include_str!("../tests/goldens/worker_prompt_mcp.txt");

    /// Regenerate with `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff: a regen is a review, not a fix.
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

    /// Section vocabulary is policy and belongs to the document; a prompt that names sections re-imposes one template's shape on every document.
    #[test]
    fn planner_prompt_carries_no_section_vocabulary() {
        let p = PLANNER_SYSTEM_PROMPT_TEMPLATE;

        // `# 进行中` must not come back via the skeleton either: the TASKS panel renders the real task runtime state.
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

        // The live section names are read off the two contract headers, so a renamed section is banned under its new name; the retired literals are not derivable and stay hand-written.
        let live = calm_types::track_report::work_brief_header()
            .sections
            .into_iter()
            .chain(calm_types::track_report::research_header().sections)
            .map(|section| format!("# {}", section.h1));
        let retired = [
            "# Goal",
            "# Progress",
            "# Needs attention",
            "# Results",
            "# Timeline",
            "# 进行中",
        ]
        .map(String::from);
        for banned in live.chain(retired) {
            assert!(
                !p.contains(&banned),
                "planner prompt must not name a report section — structure travels with the document (#1185): {banned}"
            );
        }
    }

    /// Every `calm.`-prefixed token in `text`: `calm.` not preceded by `[A-Za-z0-9_.]`, extended over `[A-Za-z0-9_.]`, trailing `.`s stripped, wildcard families (`calm.*`) dropped.
    /// Uppercase is part of the continuation so `calm.plan.listX` stays one unregistered token.
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

    /// Tokens are whole-token matched, so a misspelling or a stray suffix (`calm.plan.list2`) is red, not a prefix hit; only wildcard families are skipped.
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

    /// Every `calm.*` token in `prompt` checked against the tool registry: each must be a registered non-alias name; tokens in neither list must be visible to `role`; `callable_but_hidden` and `named_to_forbid` entries must be registered, NOT visible, and actually named by the prompt.
    /// With `must_name_all_visible` every tool visible to `role` must be named; `min_named` guards against an empty scanner only.
    fn assert_prompt_tool_names(
        label: &str,
        prompt: &str,
        role: calm_types::model::CardRole,
        callable_but_hidden: &[&str],
        named_to_forbid: &[&str],
        must_name_all_visible: bool,
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
        if must_name_all_visible {
            let unnamed: Vec<&String> = visible
                .iter()
                .filter(|name| !named.contains(name.as_str()))
                .collect();
            assert!(
                unnamed.is_empty(),
                "{label} must name every tool visible to {role:?} and does not name \
                 {unnamed:?}; the role's tool surface is not fully advertised"
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

    /// The codex prompt must name **every** tool the Worker can see: advertising only one of `calm.task.complete` / `calm.task.fail` would leave a codex worker with no way to report the other outcome. The CLI prompt completes through `neige task-completed` and is exempt.
    #[test]
    fn worker_prompts_name_only_tools_the_worker_role_can_see() {
        // `min_named` guards against an empty scanner only: the CLI prompt names exactly the two forbidden tools; the codex prompt adds the two visible completion tools.
        for (label, template, must_name_all_visible, min_named) in [
            (
                "CLI worker prompt",
                WORKER_SYSTEM_PROMPT_PLACEHOLDER,
                false,
                2,
            ),
            ("codex worker prompt", WORKER_CODEX_SYSTEM_PROMPT, true, 3),
        ] {
            assert_prompt_tool_names(
                label,
                &render_system_prompt(template, "track-registry"),
                calm_types::model::CardRole::Worker,
                &[],
                &["calm.task.dispatch", "calm.task.verdict"],
                must_name_all_visible,
                min_named,
            );
        }
    }

    /// `calm.report.read` is callable but hidden: its handler admits the Assistant while its descriptor is visible to Planner only, so the prompt is the Assistant's only contract for the read. `calm.report.write` is named to forbid it.
    /// `neige` CLI mentions are not `calm.*` tokens and are pinned only by the goldens.
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
                false,
                3,
            );
        }
    }

    /// The match is exhaustive on purpose: a new `WorkerProviderKind` variant fails to compile at the match, and every listed kind must be named by the prompt.
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

    /// Stated against the file, not a marker string, so a second copy of the tail that drifted would fail here rather than pass a `contains` check.
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
