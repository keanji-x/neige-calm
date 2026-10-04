//! The shipped SPY recipe's template slots, read from the recipe itself so no test keeps a
//! second list of the App's published kinds.

use std::collections::BTreeMap;
use std::path::Path;

use calm_types::report_blocks::native_view::ComponentKind;
use calm_types::report_blocks::{parse_fence, split_body};

pub fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/paper-trading")
            .join(name),
    )
    .unwrap()
}

/// Every live slot of the recipe's `view` fences: overlay kind -> the cell kind it expects.
pub fn slots() -> BTreeMap<String, ComponentKind> {
    let mut slots = BTreeMap::new();
    for fence in split_body(&plugin_file("spy-recipe.md"))
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == "view")
    {
        for row in fence.payload["rows"].as_array().unwrap() {
            for cell in row["cells"].as_array().unwrap() {
                let source = cell["source"].as_str().expect("a live slot");
                let kind = source.rsplit('/').next().unwrap().to_string();
                let expects = serde_json::from_value(cell["expects"].clone()).unwrap();
                assert!(
                    slots.insert(kind, expects).is_none(),
                    "{source} is placed twice"
                );
            }
        }
    }
    slots
}
