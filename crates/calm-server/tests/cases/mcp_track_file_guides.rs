//! The Planner's on-demand guides (#1893): every `neige track cat guide/<name>.md` the rendered Planner
//! prompt names is served by `neige_track_cat` with the shared task guidance rendered, and `guide/` lists
//! exactly the named set.

use std::collections::BTreeSet;
use std::path::Path;

use calm_server::operation::planner_harness_start_adapter::render_planner_developer_instructions_for_test;

use super::*;

/// The `<name>` of every `` `neige track cat guide/<name>` `` in `prompt`, in order.
fn named_guides(prompt: &str) -> Vec<String> {
    const MARK: &str = "`neige track cat guide/";
    prompt
        .match_indices(MARK)
        .map(|(at, _)| {
            let rest = &prompt[at + MARK.len()..];
            rest[..rest.find('`').expect("closing backtick")].to_string()
        })
        .collect()
}

#[tokio::test]
async fn planner_diagnostics_regression_guides_render_shared_task_rules() {
    let boot = boot().await;
    let prompt = render_planner_developer_instructions_for_test(boot.track_id.as_str(), None, None);
    let named = named_guides(&prompt);
    assert!(named.len() >= 4, "anti-vacuity: the prompt names {named:?}");

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/guides");
    for name in &named {
        let file = std::fs::read_to_string(dir.join(name))
            .unwrap_or_else(|error| panic!("prompts/guides/{name}: {error}"));
        let file = calm_types::report_blocks::tasks::render_task_guidance(&file);
        if name == "gates.md" {
            assert!(file.contains(calm_types::report_blocks::tasks::TASK_GATE_GUIDANCE));
        }
        for identity in [planner_identity(&boot), worker_identity(&boot)] {
            let served = call_tool(
                &boot,
                TOOL_TRACK_CAT,
                identity,
                json!({ "path": format!("guide/{name}") }),
            )
            .await
            .unwrap_or_else(|error| panic!("guide/{name} is not served: {error:?}"));
            assert_eq!(served["content_type"], "text/markdown", "{served}");
            assert_eq!(
                served["content"].as_str(),
                Some(file.as_str()),
                "guide/{name} must serve rendered prompts/guides/{name}"
            );
        }
    }

    let named: BTreeSet<String> = named.into_iter().collect();
    let listed = call_tool(
        &boot,
        TOOL_TRACK_LS,
        planner_identity(&boot),
        json!({ "path": "guide/" }),
    )
    .await
    .expect("guide/ lists");
    let listed: BTreeSet<String> = listed["entries"]
        .as_array()
        .expect("ls returns `entries`")
        .iter()
        .map(|entry| entry["name"].as_str().expect("entry name").to_string())
        .collect();
    assert_eq!(
        listed, named,
        "guide/ must list exactly the guides the prompt names"
    );
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("read prompts/guides")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        on_disk, named,
        "every file in prompts/guides must be named by the prompt"
    );
}
