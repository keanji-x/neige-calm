use std::collections::BTreeSet;
use std::path::PathBuf;

use syn::{Item, UseTree, Visibility};

const EXPORTED_ENTRY: &str = "guard_forked_blocks";
const PRIVATE_IMPL: &str = "guard_forked_blocks_impl";

#[test]
fn fork_rule_one_exemption_has_one_structural_entry() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes/waves/fork_guard.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    let items = &syntax.items;

    let mut exported_entries = BTreeSet::new();
    for item in items {
        match item {
            Item::Fn(function) if !matches!(function.vis, Visibility::Inherited) => {
                exported_entries.insert(function.sig.ident.to_string());
            }
            Item::Use(item_use) if !matches!(item_use.vis, Visibility::Inherited) => {
                collect_use_exports(&item_use.tree, &mut exported_entries);
            }
            _ => {}
        }
    }
    assert_eq!(
        exported_entries,
        BTreeSet::from([EXPORTED_ENTRY.to_string()]),
        "the exemption module must export exactly its fork-shaped entry"
    );

    let entry = items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == EXPORTED_ENTRY => Some(function),
        _ => None,
    });
    let entry = entry.expect("fork guard entry vanished");
    assert!(
        is_waves_only(&entry.vis),
        "{EXPORTED_ENTRY} must use exactly pub(in crate::routes::waves)"
    );

    let implementation = items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == PRIVATE_IMPL => Some(function),
        _ => None,
    });
    let implementation = implementation.expect("private fork guard implementation vanished");
    assert!(
        matches!(implementation.vis, Visibility::Inherited),
        "{PRIVATE_IMPL} must stay module-private"
    );

    assert!(
        items.iter().all(|item| !matches!(item, Item::Enum(_))),
        "the exemption module must not reintroduce a constructible exemption enum"
    );
}

fn is_waves_only(visibility: &Visibility) -> bool {
    let Visibility::Restricted(restricted) = visibility else {
        return false;
    };
    restricted.in_token.is_some()
        && restricted
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .eq(["crate", "routes", "waves"])
}

fn collect_use_exports(tree: &UseTree, exports: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_exports(&path.tree, exports),
        UseTree::Name(name) => {
            exports.insert(name.ident.to_string());
        }
        UseTree::Rename(rename) => {
            exports.insert(rename.rename.to_string());
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_exports(tree, exports);
            }
        }
        UseTree::Glob(_) => {
            exports.insert("*".into());
        }
    }
}
