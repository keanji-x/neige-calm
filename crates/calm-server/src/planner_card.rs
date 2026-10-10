//! Planner-card binding: the role-specific system prompts (data under `prompts/`, embedded at compile time) and their per-spawn placeholder substitution.

/// The planner-agent system prompt template, embedded from `prompts/planner.md`. Placeholders `{track_id}`, `{planner_wake_authors}`, and `{task_acceptance_guidance}` are substituted by [`render_system_prompt`].
/// Wording is pinned by `tests/goldens/dev_planner_prompt.txt` (regenerate with `REGEN_PLANNER_PROMPT_GOLDEN=1`, then hand-verify the diff).
pub(crate) const PLANNER_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/planner.md"),
    include_str!("../prompts/tool-discovery.md")
);

/// Worker-agent system prompt for the **claude** provider: the shared head and tail, with a one-sentence tool discovery (#2509): its neige MCP tools are already in Claude's tool list.
/// Wording is pinned by `tests/goldens/worker_prompt_claude.txt` (regenerate with `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff).
pub(crate) const WORKER_CLAUDE_SYSTEM_PROMPT: &str = concat!(
    include_str!("../prompts/worker/head.md"),
    include_str!("../prompts/worker/tool-discovery-claude.md"),
    include_str!("../prompts/worker/tail.md")
);

/// codex worker variant: the same head and tail as [`WORKER_CLAUDE_SYSTEM_PROMPT`], with the code-mode tool discovery the Planner and Assistant share. Pinned by `tests/goldens/worker_prompt_codex.txt`.
pub(crate) const WORKER_CODEX_SYSTEM_PROMPT: &str = concat!(
    include_str!("../prompts/worker/head.md"),
    include_str!("../prompts/tool-discovery.md"),
    include_str!("../prompts/worker/tail.md")
);

/// The track assistant's system prompt: ordinary head, the mechanics shared by both assistant identities, and the ordinary tail.
/// Deliberately not a trimmed planner prompt: every close/plan tool rejects `CardRole::Assistant` at the handler. Pinned by `tests/goldens/assistant_prompt.txt`.
pub(crate) const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/ordinary-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/ordinary-tail.md"),
    include_str!("../prompts/tool-discovery.md")
);

/// The assistant on Today's launchpad track: same mechanics, inverted identity (this conversation is the report's writer). Selected by `routes::today::is_launchpad_track` at `thread/start`.
/// `developer_instructions` are handed over at thread start, so an existing conversation keeps the identity it was started with. Pinned by `tests/goldens/assistant_prompt_launchpad.txt`.
pub(crate) const LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = concat!(
    include_str!("../prompts/assistant/launchpad-head.md"),
    include_str!("../prompts/assistant/mechanics.md"),
    include_str!("../prompts/assistant/launchpad-tail.md"),
    include_str!("../prompts/tool-discovery.md")
);

/// Render the report-edit authors that wake the planner, in the wire spelling the `track.report_edited` payload carries.
fn planner_wake_authors_prose() -> String {
    crate::dispatcher::PLANNER_WAKE_AUTHORS
        .iter()
        .map(|author| format!("`{}`", author.wire_str()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Substitute per-spawn identity, wake authors, and shared task acceptance guidance.
pub(crate) fn render_system_prompt(template: &str, track_id: &str) -> String {
    calm_types::observation::render_task_acceptance_guidance(
        &template
            .replace("{track_id}", track_id)
            .replace("{planner_wake_authors}", &planner_wake_authors_prose()),
    )
}

#[cfg(test)]
const TASK_BLOCK_PROTOCOL_GOLDEN: &str = concat!(
    "   * Maintain task declarations as report `task` blocks. Read the report (or the section ",
    "that holds the task) with `neige_report_read`, then create or replace the task block with ",
    "an `upsert` op of `neige_report_commit`; pass no revisions, the kernel anchors the op to ",
    "your read. To start an authorized Planner task, ",
    "its payload needs a per-track-unique ",
    "`key`, `kind` (`codex`, `claude`, or `terminal`), `ready: true`, ",
    "and `declared_by: \"spec\"`; it may also carry `acceptance`, `depends_on` ",
    "sibling keys, `priority`, and usually `gate`. Use `neige_task_cancel` to ",
    "cancel a pending task, or a running codex/claude task whose worker the kernel ",
    "then stops; dispatched, verifying and terminal-kind tasks cannot be canceled. ",
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

/// Test-only seam: the rendered codex worker prompt. Doc-hidden so it does not widen the public prompt API.
#[doc(hidden)]
pub fn render_worker_prompt_for_e2e(track_id: &str) -> String {
    render_system_prompt(SeededCardRole::WorkerCodex.prompt_template(), track_id)
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
    /// Worker card for a **claude** provider.
    WorkerClaude,
    /// Worker card for a **codex** provider.
    WorkerCodex,
}

impl SeededCardRole {
    pub(crate) fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Planner => PLANNER_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::WorkerClaude => WORKER_CLAUDE_SYSTEM_PROMPT,
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
            WORKER_CLAUDE_SYSTEM_PROMPT,
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
    fn task_acceptance_guidance_is_consistent_across_surfaces() {
        use calm_types::observation::{Observation, TASK_ACCEPTANCE_GUIDANCE};
        let guidance = TASK_ACCEPTANCE_GUIDANCE.trim();
        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "acceptance-track");
        let descriptors = crate::mcp_server::build_default_registry()
            .descriptors_listed_for(calm_types::model::CardRole::Planner);
        let descriptions = ["neige_task_accept"].map(|name| {
            descriptors
                .iter()
                .find(|tool| tool.name == name)
                .expect("Planner acceptance-related descriptor")
                .description
                .clone()
        });
        let notices = [
            Observation::TaskCompleted {
                idempotency_key: "attempt".into(),
                result: serde_json::json!({}),
            },
            Observation::TaskFailed {
                idempotency_key: "attempt".into(),
                error: "failed".into(),
            },
            Observation::TaskGitDeliverySettled {
                key: "review".into(),
                attempt_id: "attempt".into(),
                result: calm_types::git_candidate::DeliverySettlement::Candidate {
                    candidate_id: "candidate".into(),
                    commit_sha: "b".repeat(40),
                    base_sha: "b".repeat(40),
                    base_is_ancestor: true,
                },
                retained_path: None,
            },
        ];
        for text in std::iter::once(prompt)
            .chain(descriptions)
            .chain(notices.iter().map(Observation::to_turn_text))
        {
            assert!(
                text.contains(guidance),
                "acceptance guidance drifted: {text}"
            );
            assert!(!text.contains("{task_acceptance_guidance}"), "{text}");
        }
    }

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        assert_eq!(
            SeededCardRole::Planner.prompt_template(),
            PLANNER_SYSTEM_PROMPT_TEMPLATE
        );
        assert_eq!(
            SeededCardRole::WorkerClaude.prompt_template(),
            WORKER_CLAUDE_SYSTEM_PROMPT
        );
        assert_eq!(
            SeededCardRole::WorkerCodex.prompt_template(),
            WORKER_CODEX_SYSTEM_PROMPT
        );
        assert_ne!(PLANNER_SYSTEM_PROMPT_TEMPLATE, WORKER_CLAUDE_SYSTEM_PROMPT);
        assert_ne!(PLANNER_SYSTEM_PROMPT_TEMPLATE, WORKER_CODEX_SYSTEM_PROMPT);
        assert_ne!(WORKER_CLAUDE_SYSTEM_PROMPT, WORKER_CODEX_SYSTEM_PROMPT);
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
            1,
            "the wake-set sentence must carry the rendered list; got: {p}"
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
            p.contains("You are never woken by your own (`author = \"planner\"`) edits."),
            "prompt must still state the self-edit exclusion; got: {p}"
        );
    }

    const ASSISTANT_PROMPT_GOLDEN: &str = include_str!("../tests/goldens/assistant_prompt.txt");

    const LAUNCHPAD_ASSISTANT_PROMPT_GOLDEN: &str =
        include_str!("../tests/goldens/assistant_prompt_launchpad.txt");

    fn assert_assistant_prompt_golden(file: &str, rendered: &str, golden: &str) {
        if std::env::var_os("REGEN_PROMPT_GOLDENS").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/goldens")
                .join(file);
            std::fs::write(path, rendered).expect("write regenerated assistant prompt");
            panic!(
                "{file} regenerated from the current prompt; inspect and rerun without the flag"
            );
        }
        assert_eq!(rendered, golden);
    }

    /// Whole-document equality: a stray newline at either seam is what a `contains` check cannot see.
    #[test]
    fn the_ordinary_assistant_prompt_matches_its_reviewed_golden() {
        assert_assistant_prompt_golden(
            "assistant_prompt.txt",
            &render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, "track-golden-1189"),
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
        assert_assistant_prompt_golden(
            "assistant_prompt_launchpad.txt",
            &launchpad,
            LAUNCHPAD_ASSISTANT_PROMPT_GOLDEN,
        );

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

    const WORKER_PROMPT_CLAUDE_GOLDEN: &str =
        include_str!("../tests/goldens/worker_prompt_claude.txt");
    const WORKER_PROMPT_CODEX_GOLDEN: &str =
        include_str!("../tests/goldens/worker_prompt_codex.txt");

    /// Regenerate with `REGEN_PROMPT_GOLDENS=1`, then hand-verify the diff: a regen is a review, not a fix.
    #[test]
    fn the_worker_prompts_match_their_reviewed_goldens() {
        let regen = std::env::var_os("REGEN_PROMPT_GOLDENS").is_some();
        let mut mismatched = Vec::new();
        for (file, template, golden) in [
            (
                "worker_prompt_claude.txt",
                WORKER_CLAUDE_SYSTEM_PROMPT,
                WORKER_PROMPT_CLAUDE_GOLDEN,
            ),
            (
                "worker_prompt_codex.txt",
                WORKER_CODEX_SYSTEM_PROMPT,
                WORKER_PROMPT_CODEX_GOLDEN,
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
            "worker_prompt_claude.txt / worker_prompt_codex.txt regenerated from the current \
             prompts; hand-verify the diff, commit, and re-run without REGEN_PROMPT_GOLDENS"
        );
        assert!(
            mismatched.is_empty(),
            "worker prompt goldens differ from the rendered prompts: {mismatched:?}; \
             regenerate with REGEN_PROMPT_GOLDENS=1 and hand-verify the diff"
        );
    }

    /// One-sided cap (#1893): a change that shrinks `prompts/planner.md` lowers it in the same PR.
    const PLANNER_PROMPT_MAX_BYTES: usize = 7_500;

    #[test]
    fn planner_prompt_fits_its_byte_budget() {
        // A production-shaped track id (`new_id`, 32 bytes), so the budget measures what ships.
        let track = crate::model::new_id();
        assert_eq!(track.len(), 32);
        let bytes = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, &track).len();
        assert!(
            bytes >= 3_000,
            "anti-vacuity: planner.md is only {bytes} bytes"
        );
        assert!(
            bytes <= PLANNER_PROMPT_MAX_BYTES,
            "planner.md is {bytes} bytes, over its {PLANNER_PROMPT_MAX_BYTES} byte budget; \
             move situational detail to a guide or a tool description"
        );
    }

    /// Per-guide and total caps for the guides `guide/<name>.md` serves (#1893).
    #[test]
    fn every_guide_fits_its_byte_budget() {
        use crate::mcp_server::tools::track_file::GUIDES;
        const GUIDE_MAX_BYTES: usize = 6_144;
        const GUIDES_TOTAL_MAX_BYTES: usize = 7_500;

        assert!(GUIDES.len() >= 4, "anti-vacuity: {} guides", GUIDES.len());
        for (name, text) in GUIDES {
            assert!(
                (500..=GUIDE_MAX_BYTES).contains(&text.len()),
                "guide {name} is {} bytes; each guide holds 500..={GUIDE_MAX_BYTES}",
                text.len()
            );
        }
        let total: usize = GUIDES.iter().map(|(_, text)| text.len()).sum();
        assert!(
            total <= GUIDES_TOTAL_MAX_BYTES,
            "the guides total {total} bytes, over their {GUIDES_TOTAL_MAX_BYTES} byte budget"
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

    /// Every `neige_`-prefixed token in `text`: `neige_` not preceded by `[A-Za-z0-9_.]`, extended over `[A-Za-z0-9_]`, wildcard families (`neige_terminal_*`) dropped.
    /// Uppercase is part of the continuation so `neige_task_lsX` stays one unregistered token.
    fn kernel_tool_tokens(text: &str) -> Vec<&str> {
        let bytes = text.as_bytes();
        let mut tokens = Vec::new();
        let mut from = 0;
        while let Some(i) = text[from..].find("neige_") {
            let at = from + i;
            let preceded_by_ident = at > 0 && {
                let b = bytes[at - 1];
                b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
            };
            let end = at
                + bytes[at..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                    .count();
            from = end;
            if preceded_by_ident || bytes.get(end) == Some(&b'*') {
                continue;
            }
            tokens.push(&text[at..end]);
        }
        tokens
    }

    #[test]
    fn kernel_tool_tokens_are_whole_tokens() {
        for (text, expected) in [
            ("see neige_task_ls.", vec!["neige_task_ls"]),
            ("xneige_task_ls", vec![]),
            ("gitforge", vec![]),
            ("neige.kv.set", vec![]),
            ("neige_terminal_*", vec![]),
            ("neige_task_ls2", vec!["neige_task_ls2"]),
            ("neige_task_lsX", vec!["neige_task_lsX"]),
            ("", vec![]),
            ("neige_", vec!["neige_"]),
        ] {
            assert_eq!(kernel_tool_tokens(text), expected, "input: {text:?}");
        }
    }

    /// Tokens are whole-token matched, so a misspelling or a stray suffix (`neige_task_ls2`) is red, not a prefix hit; only wildcard families are skipped.
    #[test]
    fn planner_prompt_names_only_tools_the_planner_role_can_see() {
        use std::collections::BTreeSet;

        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-registry");
        let visible: BTreeSet<String> = crate::mcp_server::build_default_registry()
            .descriptors_listed_for(calm_types::model::CardRole::Planner)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        assert!(!visible.is_empty(), "the Planner role sees no tools at all");

        let named: BTreeSet<&str> = kernel_tool_tokens(&prompt).into_iter().collect();
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

    /// Every `neige_*` token in `prompt` checked against the tool registry: each must be a registered name; tokens in neither list must be visible to `role`; `callable_but_hidden` and `named_to_forbid` entries must be registered, NOT visible, and actually named by the prompt.
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
        let registered: BTreeSet<String> = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        let visible: BTreeSet<String> = registry
            .descriptors_listed_for(role)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        assert!(
            !visible.is_empty(),
            "the {role:?} role sees no tools at all"
        );

        let named: BTreeSet<&str> = kernel_tool_tokens(prompt).into_iter().collect();
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
                     (typo or retired); drop it from the list"
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
                 or not a complete tool name). Registered: {registered:?}"
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

    /// Each worker prompt must name **every** tool the Worker can see: advertising only one of `neige_task_done` / `neige_task_fail` would leave a worker with no way to report the other outcome.
    #[test]
    fn worker_prompts_name_only_tools_the_worker_role_can_see() {
        // `min_named` guards against an empty scanner only.
        for (label, template) in [
            ("claude worker prompt", WORKER_CLAUDE_SYSTEM_PROMPT),
            ("codex worker prompt", WORKER_CODEX_SYSTEM_PROMPT),
        ] {
            assert_prompt_tool_names(
                label,
                &render_system_prompt(template, "track-registry"),
                calm_types::model::CardRole::Worker,
                &[],
                &["neige_task_accept", "neige_task_reject"],
                true,
                3,
            );
        }
    }

    /// Every tool the Assistant prompts name, `neige_report_read` included (#2289), is listed for the Assistant.
    /// `neige` CLI mentions are not `neige_*` tokens and are pinned only by the goldens.
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
                &[],
                &[],
                false,
                3,
            );
        }
    }

    /// Task executor kinds come from the task contract, not every managed conversation provider.
    #[test]
    fn planner_prompt_names_every_worker_provider_kind() {
        use crate::model::TaskKind;

        let prompt = render_system_prompt(PLANNER_SYSTEM_PROMPT_TEMPLATE, "track-kinds");
        for kind in [TaskKind::Codex, TaskKind::Claude, TaskKind::Terminal] {
            match kind {
                TaskKind::Codex | TaskKind::Claude | TaskKind::Terminal => {}
            }
            let serialized = serde_json::to_value(kind).unwrap();
            let spelled = format!("`{}`", serialized.as_str().unwrap());
            assert!(
                prompt.contains(&spelled),
                "planner prompt must name task kind {spelled}"
            );
        }
    }

    /// Stated against the files, not marker strings, so a drifted second copy of the head or tail fails here rather than passing a `contains` check. The two workers differ only in tool discovery (#2509): Claude's neige tools are already in its tool list, so it gets no code-mode lookup procedure.
    #[test]
    fn worker_prompts_share_one_head_and_tail() {
        let head = include_str!("../prompts/worker/head.md");
        let tail = include_str!("../prompts/worker/tail.md");
        assert!(!head.is_empty() && !tail.is_empty());
        let discovery = |prompt: &'static str, label: &str| {
            prompt
                .strip_prefix(head)
                .and_then(|rest| rest.strip_suffix(tail))
                .unwrap_or_else(|| panic!("{label} worker prompt is head + discovery + tail"))
        };
        assert_eq!(
            discovery(WORKER_CODEX_SYSTEM_PROMPT, "codex"),
            include_str!("../prompts/tool-discovery.md")
        );
        assert_eq!(
            discovery(WORKER_CLAUDE_SYSTEM_PROMPT, "claude"),
            include_str!("../prompts/worker/tool-discovery-claude.md")
        );
        for cli_path in [
            "neige task done",
            "neige task fail",
            "neige task gate",
            "neige tool ls",
            "neige tool describe",
            "ALL_TOOLS",
            "loader",
        ] {
            assert!(
                !WORKER_CLAUDE_SYSTEM_PROMPT.contains(cli_path),
                "the claude worker prompt routes through the CLI or code-mode discovery: \
                 `{cli_path}`"
            );
        }
    }
}
