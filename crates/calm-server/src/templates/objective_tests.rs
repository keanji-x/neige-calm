use super::*;

fn source(capability: &str, copies: usize) -> &'static str {
    let payload = serde_json::json!({"key":"build","title":"Build","mode":"converge", "checks":[{"id":"tests","role":"goal","config":{"kind":"command","params":{"command":"true"}}}],"constraints":[],"budget":{"turns":10,"tokens":1000,"wall_secs":60},"compact":"none"});
    let fence = calm_types::report_blocks::render_fence("objective", &payload);
    String::leak(format!(
        "+++\nid = \"objective-test\"\ntitle = \"Objective\"\n{capability}\n+++\n{}",
        fence.repeat(copies)
    ))
}
#[test]
fn objective_roster_requires_capability_and_unique_keys() {
    assert!(TemplateRoster::from_sources(&[source("", 1)]).is_err());
    assert!(TemplateRoster::from_sources(&[source("capabilities = [\"objectives\"]", 2)]).is_err());
    assert!(TemplateRoster::from_sources(&[source("capabilities = [\"objectives\"]", 1)]).is_ok());
}

#[test]
fn objective_long_term_goal_template_declares_budget_slot() {
    let template = TemplateRoster::builtin().get(LONG_TERM_GOAL).unwrap();
    assert_eq!(
        template.capabilities(),
        &[calm_types::track_capability::TrackCapability::Objectives]
    );
    let recipe = template.recipe();
    let start = recipe.body.find("```neige-block view\n").unwrap() + "```neige-block view\n".len();
    let end = recipe.body[start..].find("\n```").unwrap() + start;
    let payload: Value = serde_json::from_str(&recipe.body[start..end]).unwrap();
    let parsed: calm_types::report_blocks::native_view::NativeView =
        serde_json::from_value(payload).unwrap();
    assert!(
        serde_json::to_string(&parsed)
            .unwrap()
            .contains("neige://plugin/kernel/objective.budget")
    );
}
