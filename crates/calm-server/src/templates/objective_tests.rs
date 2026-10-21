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
