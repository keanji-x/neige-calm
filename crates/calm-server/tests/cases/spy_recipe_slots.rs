//! The shipped SPY recipe's template slots, read from the recipe itself so no test keeps a
//! second list of the App's published kinds.

use std::collections::BTreeMap;
use std::path::Path;

use calm_types::report_blocks::native_view::{ComponentKind, NativeView, RowCell};
use calm_types::report_blocks::{KIND_VIEW, parse_fence, split_body};
use serde::Deserialize;

pub fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/paper-trading")
            .join(name),
    )
    .unwrap()
}

/// Every live slot of the recipe's `view` fences: overlay kind -> the cell kind it expects. Each
/// fence decodes as the kernel decodes it (`track_report_hydrate.rs`); inline cells are skipped.
pub fn slots() -> BTreeMap<String, ComponentKind> {
    let mut slots = BTreeMap::new();
    for fence in split_body(&plugin_file("spy-recipe.md"))
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == KIND_VIEW)
    {
        let view = NativeView::deserialize(&fence.payload).expect("a recipe view fence");
        for cell in view.rows.into_iter().flat_map(|row| row.cells) {
            let RowCell::Live(slot) = cell else { continue };
            let kind = slot.source.rsplit('/').next().unwrap().to_string();
            assert!(
                slots.insert(kind, slot.expects).is_none(),
                "{} is placed twice",
                slot.source
            );
        }
    }
    slots
}
