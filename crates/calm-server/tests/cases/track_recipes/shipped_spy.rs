//! The shipped SPY recipe and its example against the kernel's own validators: the recipe
//! ingress admits the template, and every example unit passes `validate_unit` for its slot.

use std::collections::BTreeSet;
use std::path::Path;

use calm_types::report_blocks::native_view::{ComponentKind, validate, validate_unit};

use super::*;

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/paper-trading")
            .join(name),
    )
    .unwrap()
}

#[tokio::test]
async fn the_shipped_spy_recipe_is_admitted_with_its_template_views_intact() {
    let boot = boot().await;
    let body = plugin_file("spy-recipe.md");
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/track-recipes",
        Some("user"),
        Some(json!({ "title": "spy", "body": body })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let stored = created["body"].as_str().expect("body");

    // Normalization re-renders the fences; the template views survive unchanged.
    let views = fences(stored);
    assert_eq!(views, fences(&body), "{stored}");
    assert_eq!(views.len(), 3);
    assert!(views.iter().all(|(kind, _)| kind == "view"), "{views:?}");

    let header = calm_types::report_contract::check_document(stored)
        .unwrap()
        .expect("contract header");
    let declared: Vec<String> = header.sections.into_iter().map(|s| s.h1).collect();
    let h1s: Vec<String> = headings(stored)
        .into_iter()
        .filter(|(level, _)| *level == 1)
        .map(|(_, text)| text)
        .collect();
    // The plugin's own tests pin which H1s these are; here the kernel's parse must agree.
    assert_eq!(h1s, declared);
}

#[test]
fn every_slot_of_the_committed_spy_example_resolves_a_valid_unit() {
    let example: Value = serde_json::from_str(&plugin_file("examples/native-demo.json")).unwrap();
    let overlays = example["overlays"].as_object().expect("overlays");
    let mut used = BTreeSet::new();
    for view in example["views"].as_array().expect("views") {
        validate(view).unwrap_or_else(|error| panic!("template view: {error}"));
        for cell in view["rows"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row["cells"].as_array().unwrap())
        {
            let source = cell["source"].as_str().expect("a live slot");
            let kind = source.rsplit('/').next().unwrap();
            let expects: ComponentKind = serde_json::from_value(cell["expects"].clone()).unwrap();
            let unit = overlays
                .get(kind)
                .unwrap_or_else(|| panic!("{kind} is not published"));
            validate_unit(expects, unit).unwrap_or_else(|error| panic!("{kind}: {error}"));
            used.insert(kind.to_string());
        }
    }
    assert_eq!(used, overlays.keys().cloned().collect::<BTreeSet<_>>());
}
