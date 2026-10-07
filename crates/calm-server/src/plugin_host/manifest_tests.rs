//! Server-side tests of shipped templates and planner rendering.
use super::manifest::*;
use serde_json::json;

const DEV_RENDERED_PROMPT_GOLDEN: &str = include_str!("../../tests/goldens/dev_planner_prompt.txt");

fn assert_full_golden_eq(expected: &str, actual: &str) {
    assert!(
        !expected.is_empty(),
        "full golden degenerate state: expected golden must not be empty"
    );
    assert!(
        !actual.is_empty(),
        "full golden degenerate state: rendered output must not be empty"
    );
    if expected == actual {
        return;
    }

    let first_difference = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(expected, actual)| expected != actual)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let mut context_offset = first_difference;
    while !expected.is_char_boundary(context_offset) || !actual.is_char_boundary(context_offset) {
        context_offset -= 1;
    }

    fn line_context(text: &str, byte_offset: usize) -> String {
        let line_start = text[..byte_offset].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[byte_offset..]
            .find('\n')
            .map_or(text.len(), |index| byte_offset + index);
        let line_number = text[..line_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        let column = text[line_start..byte_offset].chars().count() + 1;
        format!(
            "line {line_number}, column {column}: {:?}",
            &text[line_start..line_end]
        )
    }

    panic!(
        "full golden mismatch at byte {first_difference} (expected {} bytes, actual {} bytes)\n\
             expected next {:?}; {}\n  actual next {:?}; {}",
        expected.len(),
        actual.len(),
        expected[context_offset..].chars().next(),
        line_context(expected, context_offset),
        actual[context_offset..].chars().next(),
        line_context(actual, context_offset)
    );
}

#[test]
#[should_panic(expected = "full golden degenerate state")]
fn full_golden_equality_rejects_empty_expected_and_actual() {
    assert_full_golden_eq("", "");
}

#[test]
fn shipped_git_forge_give_up_uses_the_track_close_tool() {
    Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
        .expect("shipped git-forge manifest");
    let descriptor = crate::mcp_server::build_default_registry()
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.name == "neige_track_close")
        .expect("GIVE-UP tool descriptor");
    assert!(
        descriptor.roles == [crate::model::CardRole::Planner],
        "only the Planner closes a track: {:?}",
        descriptor.roles
    );

    let template = TemplateDescriptor { id: "dev".into() };
    let rendered =
        crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
            "track-give-up",
            Some(&template),
            None,
        );
    crate::planner_card::validate_planner_prompt_contract(&rendered)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !rendered.contains("If n == cap and the round is non-approving"),
        "S5 descriptor has no planner_instructions to inject"
    );
}

#[test]
fn shipped_dev_rendered_prompt_matches_full_golden() {
    let manifest = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
        .expect("shipped git-forge manifest");
    let template = manifest
        .templates
        .iter()
        .find(|template| template.id == "dev")
        .expect("dev template");

    // A legal final state for the shipped schema, with every required and optional field populated.
    let template_input = json!({
        "issue_url": "https://github.com/neige-calm/neige-calm/issues/985",
        "repo": "neige-calm/neige-calm",
        "issue_number": 985,
        "merge_policy": "auto-merge",
        "notes": "Full golden fixture covers every shipped template input field."
    });
    crate::plugin_host::template_input::validate_template_input(
        manifest
            .input_schema
            .as_ref()
            .expect("shipped git-forge Manifest.input_schema"),
        &template_input,
    )
    .expect("full golden template_input satisfies the shipped schema");
    let rendered =
        crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
            "track-golden-985",
            Some(template),
            Some(&template_input),
        );

    if std::env::var_os("REGEN_PLANNER_PROMPT_GOLDEN").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens/dev_planner_prompt.txt");
        // Write back `rendered + "\n"`: the assertion side does
        // `strip_suffix('\n')`, so omitting it panics on the very next run.
        std::fs::write(&path, format!("{rendered}\n")).expect("write regenerated golden");
        panic!(
            "dev_planner_prompt.txt regenerated from the current prompt; \
                 hand-verify the diff, commit, and re-run without REGEN_PLANNER_PROMPT_GOLDEN"
        );
    }

    let expected = DEV_RENDERED_PROMPT_GOLDEN
        .strip_suffix('\n')
        .expect("text fixture has its repository newline");
    assert_full_golden_eq(expected, &rendered);
}
