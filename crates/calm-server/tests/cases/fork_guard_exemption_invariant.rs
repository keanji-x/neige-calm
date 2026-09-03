use std::collections::BTreeSet;
use std::path::PathBuf;

use syn::{Item, ItemMod, UseTree, Visibility};

const EXPORTED_ENTRY: &str = "guard_forked_blocks";
const PRIVATE_IMPL: &str = "guard_forked_blocks_impl";

#[test]
fn fork_rule_one_exemption_has_one_structural_entry() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes/tracks/fork_guard.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    let items = &syntax.items;

    let entry = items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == EXPORTED_ENTRY => Some(function),
        _ => None,
    });
    let entry = entry.expect("fork guard entry vanished");
    assert!(
        is_not_wider_than_tracks(&entry.vis, 0),
        "{EXPORTED_ENTRY} must not be visible beyond crate::routes::tracks"
    );

    let mut exported_entries = BTreeSet::new();
    collect_module_exports(items, "fork_guard", &mut exported_entries);
    collect_parent_reexports(&mut exported_entries);
    let expected_entries = if matches!(entry.vis, Visibility::Inherited) {
        BTreeSet::new()
    } else {
        BTreeSet::from([format!("fork_guard::{EXPORTED_ENTRY}")])
    };
    assert_eq!(
        exported_entries, expected_entries,
        "the exemption module must export exactly its fork-shaped entry"
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

/// INV-1110-005 (partial, S5): `TemplateDescriptor` is an id handle. Do not
/// grow a public descriptor body (plan_template / gates / spec_instructions /
/// card_kinds / leftover input_schema) or add sibling public template types.
#[test]
fn template_descriptor_surface_is_id_only() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/plugin_host/manifest.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    let descriptor = syntax.items.iter().find_map(|item| match item {
        Item::Struct(item_struct) if item_struct.ident == "TemplateDescriptor" => Some(item_struct),
        _ => None,
    });
    let descriptor = descriptor.expect("TemplateDescriptor vanished from the manifest parser");
    assert!(
        matches!(descriptor.vis, Visibility::Public(_)),
        "TemplateDescriptor must stay pub so track-create can resolve plugin_scope"
    );

    let mut expected_entries = BTreeSet::new();
    expected_entries.insert("id".to_string());
    let mut fields = BTreeSet::new();
    for field in &descriptor.fields {
        let name = field
            .ident
            .as_ref()
            .expect("TemplateDescriptor must be a named struct")
            .to_string();
        fields.insert(name);
    }
    assert_eq!(
        fields, expected_entries,
        "TemplateDescriptor must stay {{ id }} (#1110 S5)"
    );

    let mut public_template_types = BTreeSet::new();
    for item in &syntax.items {
        let (ident, vis) = match item {
            Item::Struct(item_struct) => (&item_struct.ident, &item_struct.vis),
            Item::Enum(item_enum) => (&item_enum.ident, &item_enum.vis),
            Item::Type(item_type) => (&item_type.ident, &item_type.vis),
            _ => continue,
        };
        if matches!(vis, Visibility::Inherited) {
            continue;
        }
        let name = ident.to_string();
        if name == "TemplateDescriptor" {
            continue;
        }
        // Both spellings: #1268 renamed the type, but the ban has to keep
        // catching a type reintroduced under the retired prefix — otherwise
        // the rename would have quietly reopened the hole this test closes.
        if name.contains("Workflow") || name.contains("Template") || name.ends_with("Descriptor") {
            public_template_types.insert(name);
        }
    }
    assert!(
        public_template_types.is_empty(),
        "new public template-descriptor types must not appear: {public_template_types:?}"
    );
}

fn is_not_wider_than_tracks(visibility: &Visibility, inline_depth: usize) -> bool {
    let Visibility::Restricted(restricted) = visibility else {
        return matches!(visibility, Visibility::Inherited);
    };
    let segments: Vec<_> = restricted
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match segments.first().map(String::as_str) {
        Some("self") => true,
        Some("super") => {
            segments.iter().all(|segment| segment == "super") && segments.len() <= inline_depth + 1
        }
        Some("crate") => segments.starts_with(&[
            "crate".to_string(),
            "routes".to_string(),
            "tracks".to_string(),
        ]),
        _ => false,
    }
}

fn collect_module_exports(items: &[Item], module: &str, exports: &mut BTreeSet<String>) {
    for item in items {
        match item {
            Item::Fn(function) if !matches!(function.vis, Visibility::Inherited) => {
                exports.insert(format!("{module}::{}", function.sig.ident));
            }
            Item::Use(item_use) if !matches!(item_use.vis, Visibility::Inherited) => {
                collect_use_exports(&item_use.tree, module, exports);
            }
            Item::Mod(item_mod) if !is_cfg_test_module(item_mod) => {
                if let Some((_, items)) = &item_mod.content {
                    collect_module_exports(
                        items,
                        &format!("{module}::{}", item_mod.ident),
                        exports,
                    );
                }
            }
            _ => {}
        }
    }
}

fn is_cfg_test_module(module: &ItemMod) -> bool {
    module.ident == "tests"
        && module.attrs.iter().any(|attribute| {
            attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|argument| argument == "test")
        })
}

fn collect_parent_reexports(exports: &mut BTreeSet<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes/tracks.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    for item in &syntax.items {
        let Item::Use(item_use) = item else {
            continue;
        };
        if matches!(item_use.vis, Visibility::Inherited) {
            continue;
        }
        let mut paths = Vec::new();
        collect_use_paths(&item_use.tree, &mut Vec::new(), &mut paths);
        for (path, exported_name) in paths {
            if path.iter().any(|segment| segment == "fork_guard") {
                exports.insert(format!("tracks::{exported_name}"));
            }
        }
    }
}

fn collect_use_exports(tree: &UseTree, module: &str, exports: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_exports(&path.tree, module, exports),
        UseTree::Name(name) => {
            exports.insert(format!("{module}::{}", name.ident));
        }
        UseTree::Rename(rename) => {
            exports.insert(format!("{module}::{}", rename.rename));
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_exports(tree, module, exports);
            }
        }
        UseTree::Glob(_) => {
            exports.insert(format!("{module}::*"));
        }
    }
}

fn collect_use_paths(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    paths: &mut Vec<(Vec<String>, String)>,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_paths(&path.tree, prefix, paths);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            paths.push((path, name.ident.to_string()));
        }
        UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            paths.push((path, rename.rename.to_string()));
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_paths(tree, prefix, paths);
            }
        }
        UseTree::Glob(_) => paths.push((prefix.clone(), "*".into())),
    }
}
